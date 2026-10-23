//! Manifest declarations with bounded local reference traversal and no media or key reads.

use super::{
    Evidence, Finding, InputFormat, Inspection, InspectionError, MAX_MANIFEST_BYTES, Protection,
    Track, TrackKind, read_manifest,
};
use crate::m3u8::{Attributes, parse_attributes};
use roxmltree::{Document, Node, NodeId};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

const DASH_NS: &str = "urn:mpeg:dash:schema:mpd:2011";
const CENC_NS: &str = "urn:mpeg:cenc:2013";
const MP4_PROTECTION: &str = "urn:mpeg:dash:mp4protection:2011";
const MAX_XML_NODES: u32 = 20_000;
const MAX_TRACKS: usize = 4_096;
const MAX_PLAYLISTS: usize = 32;
const MAX_REFERENCES: usize = 128;
const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
const MAX_LINES: usize = 65_536;
const MAX_KEYS_PER_TRACK: usize = 64;
const MAX_PROTECTION_DECLARATIONS: usize = 64;

fn finding(findings: &mut Vec<Finding>, code: &'static str) {
    if !findings.iter().any(|item| item.code == code) {
        findings.push(Finding::new(code));
    }
}

fn text(bytes: &[u8]) -> Result<&str, InspectionError> {
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(InspectionError("manifest-size-limit"));
    }
    std::str::from_utf8(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes))
        .map_err(|_| InspectionError("manifest-invalid-utf8"))
}

fn track(protection: Protection) -> Track {
    Track {
        id: String::new(),
        kind: TrackKind::Unknown,
        codec: None,
        protection,
        key_ids: Vec::new(),
        pattern: None,
    }
}

fn safe_codec(value: &str) -> Option<String> {
    (!value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b".,_-".contains(&c)))
    .then(|| value.to_string())
}

fn key_id(value: &str) -> Option<String> {
    if !matches!(value.len(), 32 | 36) {
        return None;
    }
    let compact: String = value.chars().filter(|c| *c != '-').collect();
    (compact.len() == 32 && compact.bytes().all(|c| c.is_ascii_hexdigit()))
        .then(|| compact.to_ascii_lowercase())
}

fn drm_name(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed" => Some("widevine"),
        "urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95" | "com.microsoft.playready" => {
            Some("playready")
        }
        "urn:uuid:94ce86fb-07ff-4f43-adb8-93d2fa968ca2" | "com.apple.streamingkeydelivery" => {
            Some("fairplay")
        }
        "urn:uuid:e2719d58-a985-b3c9-781a-b030af78d30e" => Some("clearkey"),
        _ => None,
    }
}

fn kind(value: &str) -> TrackKind {
    if value == "video" || value.starts_with("video/") {
        TrackKind::Video
    } else if value == "audio" || value.starts_with("audio/") {
        TrackKind::Audio
    } else if matches!(
        value,
        "text" | "subtitle" | "application/ttml+xml" | "text/vtt"
    ) {
        TrackKind::Text
    } else {
        TrackKind::Unknown
    }
}

fn inherited<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.ancestors()
        .filter(Node::is_element)
        .find_map(|ancestor| ancestor.attribute(name))
}

fn dash_protection(
    cp: Node<'_, '_>,
    schemes: &mut Vec<Protection>,
    systems: &mut Vec<String>,
    findings: &mut Vec<Finding>,
) {
    let scheme = cp.attribute("schemeIdUri").unwrap_or("");
    if scheme.len() > 256 || scheme.chars().any(char::is_control) {
        schemes.push(Protection::Unknown);
        finding(findings, "dash-unrecognized-content-protection");
        return;
    }
    let scheme = scheme.to_ascii_lowercase();
    if scheme == MP4_PROTECTION {
        let protection = match cp.attribute("value") {
            Some("cenc") => Protection::Cenc,
            Some("cbcs") => Protection::Cbcs,
            Some("cens") => Protection::Cens,
            Some("cbc1") => Protection::Cbc1,
            _ => {
                finding(findings, "dash-unknown-protection-scheme");
                Protection::Unknown
            }
        };
        schemes.push(protection);
    } else if let Some(system) = drm_name(&scheme) {
        if !systems.iter().any(|known| known == system) {
            systems.push(system.into());
        }
    } else {
        finding(findings, "dash-unrecognized-content-protection");
        if scheme.is_empty() {
            schemes.push(Protection::Unknown);
        }
    }
}

fn inspect_representation(
    node: Node<'_, '_>,
    namespace: Option<&str>,
    invalid_scopes: &HashSet<NodeId>,
    systems: &mut Vec<String>,
    findings: &mut Vec<Finding>,
) -> Result<Track, InspectionError> {
    let mut result = track(Protection::Clear);
    result.kind = inherited(node, "contentType")
        .or_else(|| inherited(node, "mimeType"))
        .map_or(TrackKind::Unknown, kind);
    if let Some(codec) = inherited(node, "codecs") {
        result.codec = safe_codec(codec);
        if result.codec.is_none() {
            finding(findings, "invalid-codec-label");
        }
    }
    let mut schemes = Vec::new();
    let mut protected = false;
    let mut declarations = 0;
    let mut key_values = 0;
    for ancestor in node.ancestors().filter(Node::is_element) {
        if invalid_scopes.contains(&ancestor.id()) {
            protected = true;
            schemes.push(Protection::Unknown);
            finding(findings, "dash-unsupported-protection-location");
        }
        for cp in ancestor
            .children()
            .filter(|child| child.is_element() && child.tag_name().name() == "ContentProtection")
        {
            declarations += 1;
            if declarations > MAX_PROTECTION_DECLARATIONS {
                return Err(InspectionError("manifest-protection-limit"));
            }
            protected = true;
            if cp.tag_name().namespace() != namespace {
                schemes.push(Protection::Unknown);
                finding(findings, "dash-unsupported-namespace");
                continue;
            }
            dash_protection(cp, &mut schemes, systems, findings);
            for attribute in cp.attributes().filter(|a| {
                a.name() == "default_KID" && matches!(a.namespace(), None | Some(CENC_NS))
            }) {
                if attribute.value().len() > MAX_KEYS_PER_TRACK * 37 {
                    return Err(InspectionError("manifest-key-limit"));
                }
                for value in attribute.value().split_ascii_whitespace() {
                    key_values += 1;
                    if key_values > MAX_KEYS_PER_TRACK {
                        return Err(InspectionError("manifest-key-limit"));
                    }
                    if let Some(kid) = key_id(value) {
                        result.key_ids.push(kid);
                    } else {
                        finding(findings, "invalid-key-id");
                    }
                }
            }
        }
    }
    if protected {
        result.protection = Protection::aggregate(schemes);
    }
    result.key_ids.sort();
    result.key_ids.dedup();
    Ok(result)
}

fn dash_period_count(root: Node<'_, '_>, namespace: Option<&str>) -> super::PeriodCount {
    let mut count = 0;
    for period in root
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "Period")
    {
        if period.parent() != Some(root) || period.tag_name().namespace() != namespace {
            return super::PeriodCount {
                status: super::FactStatus::Unknown,
                count: None,
            };
        }
        count += 1;
    }
    super::PeriodCount {
        status: super::FactStatus::Known,
        count: Some(count),
    }
}

pub(super) fn inspect_dash(bytes: &[u8]) -> Result<Inspection, InspectionError> {
    let text = text(bytes)?;
    if text.contains("<!DOCTYPE") {
        return Err(InspectionError("xml-doctype-forbidden"));
    }
    let options = roxmltree::ParsingOptions {
        nodes_limit: MAX_XML_NODES,
        ..roxmltree::ParsingOptions::default()
    };
    let document = Document::parse_with_options(text, options)
        .map_err(|_| InspectionError("dash-invalid-xml"))?;
    let root = document.root_element();
    if root.tag_name().name() != "MPD"
        || !matches!(root.tag_name().namespace(), None | Some(DASH_NS))
    {
        return Err(InspectionError("dash-unsupported-root"));
    }
    let namespace = root.tag_name().namespace();
    // Associate unsupported placements with their nearest presentation scope once,
    // rather than walking every representation's inherited subtree repeatedly.
    let mut invalid_scopes = HashSet::new();
    for cp in root
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "ContentProtection")
    {
        if cp.ancestors().take(65).count() > 64 {
            return Err(InspectionError("manifest-depth-limit"));
        }
        if let Some(scope) = cp.ancestors().skip(1).find(|node| {
            node.is_element()
                && node.tag_name().namespace() == namespace
                && matches!(
                    node.tag_name().name(),
                    "MPD" | "Period" | "AdaptationSet" | "Representation"
                )
        }) && (cp.parent() != Some(scope)
            || !matches!(scope.tag_name().name(), "AdaptationSet" | "Representation"))
        {
            invalid_scopes.insert(scope.id());
        }
    }
    let mut tracks = Vec::new();
    let mut systems = Vec::new();
    let mut findings = Vec::new();
    for node in root.descendants().filter(Node::is_element) {
        if node.ancestors().take(65).count() > 64 {
            return Err(InspectionError("manifest-depth-limit"));
        }
        if node.tag_name().name() != "Representation" {
            continue;
        }
        let hierarchy = ["Representation", "AdaptationSet", "Period", "MPD"];
        let valid_hierarchy = node
            .ancestors()
            .filter(Node::is_element)
            .zip(hierarchy)
            .all(|(ancestor, name)| {
                ancestor.tag_name().name() == name && ancestor.tag_name().namespace() == namespace
            })
            && node.ancestors().filter(Node::is_element).count() == hierarchy.len();
        if valid_hierarchy {
            tracks.push(inspect_representation(
                node,
                namespace,
                &invalid_scopes,
                &mut systems,
                &mut findings,
            )?);
        } else {
            finding(&mut findings, "dash-unsupported-namespace");
            tracks.push(track(Protection::Unknown));
        }
        if tracks.len() > MAX_TRACKS {
            return Err(InspectionError("manifest-track-limit"));
        }
    }
    if tracks.is_empty() {
        finding(&mut findings, "dash-no-representations");
    }
    for (index, item) in tracks.iter_mut().enumerate() {
        item.id = format!("dash-{index:04}");
    }
    Ok(Inspection::new(
        InputFormat::Dash,
        Evidence::Manifest,
        tracks,
        systems,
        findings,
    )
    .with_details(dash_period_count(root, namespace)))
}

#[derive(Clone)]
struct Reference {
    uri: String,
    kind: TrackKind,
    codec: Option<String>,
}

#[derive(Clone)]
struct Key {
    method: String,
    valid: bool,
    iv: bool,
    kid: Option<String>,
}

#[derive(Default)]
struct HlsDocument {
    references: Vec<Reference>,
    keys: BTreeMap<String, Key>,
    protections: Vec<Protection>,
    key_ids: BTreeSet<String>,
    systems: Vec<String>,
    findings: Vec<Finding>,
    has_media: bool,
    pending_duration: bool,
    pending_variant: Option<Reference>,
    methods: BTreeSet<String>,
}

fn attributes(value: &str) -> Result<Attributes, InspectionError> {
    if value.is_empty() {
        return Err(InspectionError("hls-invalid-attributes"));
    }
    parse_attributes(value).map_err(|_| InspectionError("hls-invalid-attributes"))
}

impl HlsDocument {
    fn key(&mut self, value: &str, session: bool) -> Result<(), InspectionError> {
        let attrs = attributes(value)?;
        let method = attrs.get("METHOD").unwrap_or("");
        let format = attrs.get("KEYFORMAT").unwrap_or("identity");
        if format.is_empty() || format.len() > 256 || format.chars().any(char::is_control) {
            return Err(InspectionError("hls-invalid-key-format"));
        }
        if let Some(system) = drm_name(format)
            && !self.systems.iter().any(|known| known == system)
        {
            self.systems.push(system.into());
        }
        if session {
            finding(&mut self.findings, "hls-session-key-not-media-evidence");
            return Ok(());
        }
        if method == "NONE" && attrs.len() == 1 {
            self.keys.clear();
            return Ok(());
        }
        let iv = attrs.get("IV");
        let valid_iv = iv.is_none_or(|value| {
            value.strip_prefix("0x").is_some_and(|hex| {
                !hex.is_empty() && hex.len() <= 32 && hex.bytes().all(|c| c.is_ascii_hexdigit())
            })
        });
        let kid = attrs
            .get("KEYID")
            .and_then(|id| key_id(id.strip_prefix("0x").unwrap_or(id)));
        let valid = valid_iv
            && attrs
                .get("URI")
                .is_some_and(|uri| !uri.is_empty() && !uri.chars().any(char::is_control))
            && matches!(method, "AES-128" | "SAMPLE-AES" | "SAMPLE-AES-CTR");
        if !valid {
            finding(&mut self.findings, "hls-unknown-key-declaration");
        }
        if attrs.get("KEYID").is_some() && kid.is_none() {
            finding(&mut self.findings, "invalid-key-id");
        }
        if self.keys.len() >= MAX_KEYS_PER_TRACK && !self.keys.contains_key(format) {
            return Err(InspectionError("manifest-key-limit"));
        }
        self.keys.insert(
            format.to_string(),
            Key {
                method: method.into(),
                valid,
                iv: iv.is_some(),
                kid,
            },
        );
        Ok(())
    }
    fn current_protection(&mut self) -> Protection {
        if self.keys.is_empty() {
            self.methods.insert("NONE".into());
            return Protection::Clear;
        }
        for key in self.keys.values() {
            self.methods.insert(if key.valid {
                key.method.clone()
            } else {
                "UNKNOWN".into()
            });
        }
        if self
            .keys
            .values()
            .any(|key| matches!(key.method.as_str(), "SAMPLE-AES" | "SAMPLE-AES-CTR"))
        {
            finding(&mut self.findings, "sample-encryption-container-unverified");
        }
        let values = self
            .keys
            .values()
            .map(|key| {
                if key.valid && key.method == "AES-128" {
                    Protection::HlsAes128
                } else {
                    Protection::Unknown
                }
            })
            .collect::<Vec<_>>();
        if values.contains(&Protection::Unknown) {
            finding(&mut self.findings, "hls-protection-unresolved");
        }
        Protection::aggregate(values)
    }
    fn reference(&mut self, attrs: &Attributes, role: TrackKind) -> Result<(), InspectionError> {
        let uri = attrs
            .get("URI")
            .ok_or(InspectionError("hls-reference-missing"))?;
        self.references.push(Reference {
            uri: uri.into(),
            kind: role,
            codec: attrs.get("CODECS").and_then(safe_codec),
        });
        Ok(())
    }
    fn remember_keys(&mut self) -> Result<(), InspectionError> {
        for kid in self.keys.values().filter_map(|key| key.kid.as_ref()) {
            if !self.key_ids.contains(kid) {
                if self.key_ids.len() >= MAX_KEYS_PER_TRACK {
                    return Err(InspectionError("manifest-key-limit"));
                }
                self.key_ids.insert(kid.clone());
            }
        }
        Ok(())
    }
    fn segment(&mut self, uri: &str) -> Result<(), InspectionError> {
        if let Some(mut reference) = self.pending_variant.take() {
            reference.uri = uri.into();
            self.references.push(reference);
            return Ok(());
        }
        self.has_media = true;
        let protection = if self.pending_duration {
            self.current_protection()
        } else {
            finding(&mut self.findings, "hls-segment-missing-duration");
            self.methods.insert("UNKNOWN".into());
            Protection::Unknown
        };
        self.protections.push(protection);
        self.remember_keys()?;
        self.pending_duration = false;
        Ok(())
    }
    fn map(&mut self, value: &str) -> Result<(), InspectionError> {
        let attrs = attributes(value)?;
        if attrs.get("URI").is_none() {
            return Err(InspectionError("hls-map-uri-missing"));
        }
        if !self.keys.is_empty() {
            let protection = self.current_protection();
            self.protections.push(protection);
            self.remember_keys()?;
        }
        if self
            .keys
            .values()
            .any(|key| key.method == "AES-128" && !key.iv)
        {
            self.protections.push(Protection::Unknown);
            finding(&mut self.findings, "hls-encrypted-map-iv-missing");
        }
        Ok(())
    }
    fn line(&mut self, line: &str) -> Result<(), InspectionError> {
        let (name, value) = line.split_once(':').unwrap_or((line, ""));
        match name {
            "#EXT-X-KEY" => self.key(value, false)?,
            "#EXT-X-SESSION-KEY" => self.key(value, true)?,
            "#EXT-X-MAP" => self.map(value)?,
            "#EXT-X-PART" => {
                attributes(value)?;
                self.has_media = true;
                self.protections.push(Protection::Unknown);
                self.methods.insert("UNKNOWN".into());
                finding(&mut self.findings, "hls-partial-segments-unverified");
            }
            "#EXT-X-STREAM-INF" => {
                if self.pending_variant.is_some() {
                    return Err(InspectionError("hls-reference-missing"));
                }
                let attrs = attributes(value)?;
                if !attrs
                    .get("BANDWIDTH")
                    .is_some_and(|value| value.parse::<u64>().is_ok_and(|number| number > 0))
                {
                    return Err(InspectionError("hls-invalid-bandwidth"));
                }
                self.pending_variant = Some(Reference {
                    uri: String::new(),
                    kind: TrackKind::Video,
                    codec: attrs.get("CODECS").and_then(safe_codec),
                });
            }
            "#EXT-X-MEDIA" => {
                let attrs = attributes(value)?;
                let role = match attrs.get("TYPE") {
                    Some("AUDIO") => TrackKind::Audio,
                    Some("SUBTITLES") => TrackKind::Text,
                    Some("CLOSED-CAPTIONS") => return Ok(()),
                    _ => return Err(InspectionError("hls-invalid-media-type")),
                };
                if attrs.get("URI").is_some() {
                    self.reference(&attrs, role)?;
                }
            }
            "#EXT-X-I-FRAME-STREAM-INF" => self.reference(&attributes(value)?, TrackKind::Video)?,
            "#EXTINF" => {
                let duration = value
                    .split_once(',')
                    .ok_or(InspectionError("hls-invalid-duration"))?
                    .0;
                if self.pending_duration
                    || !duration
                        .parse::<f64>()
                        .is_ok_and(|number| number.is_finite() && number >= 0.0)
                {
                    return Err(InspectionError("hls-invalid-duration"));
                }
                self.pending_duration = true;
            }
            _ if !line.starts_with('#') => self.segment(line)?,
            _ => {}
        }
        Ok(())
    }
}

fn hls_document(bytes: &[u8]) -> Result<HlsDocument, InspectionError> {
    let text = text(bytes)?;
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    if lines.next() != Some("#EXTM3U") {
        return Err(InspectionError("hls-invalid-header"));
    }
    let mut document = HlsDocument::default();
    for (index, line) in lines.enumerate() {
        if index >= MAX_LINES {
            return Err(InspectionError("manifest-line-limit"));
        }
        if line.contains('\0') {
            return Err(InspectionError("hls-invalid-line"));
        }
        document.line(line)?;
        if document.references.len() > MAX_REFERENCES {
            return Err(InspectionError("hls-reference-limit"));
        }
    }
    if document.pending_variant.is_some() {
        return Err(InspectionError("hls-reference-missing"));
    }
    if document.pending_duration {
        return Err(InspectionError("hls-segment-missing"));
    }
    if !document.references.is_empty() && document.has_media {
        return Err(InspectionError("hls-ambiguous-playlist"));
    }
    Ok(document)
}

struct Traversal {
    root: PathBuf,
    active: HashSet<PathBuf>,
    cache: HashMap<PathBuf, Inspection>,
    references: usize,
    bytes: u64,
}

impl Traversal {
    fn unresolved(code: &'static str) -> Inspection {
        Inspection::new(
            InputFormat::Hls,
            Evidence::Manifest,
            vec![track(Protection::Unknown)],
            Vec::new(),
            vec![Finding::new(code)],
        )
    }
    fn child(
        &mut self,
        parent: &Path,
        reference: &Reference,
    ) -> Result<Inspection, InspectionError> {
        self.references += 1;
        if self.references > MAX_REFERENCES {
            return Err(InspectionError("hls-reference-limit"));
        }
        if reference.uri.contains("://") || reference.uri.starts_with("//") {
            return Ok(Self::unresolved("hls-remote-reference"));
        }
        let relative = reference.uri.split(['?', '#']).next().unwrap_or("");
        if relative.is_empty()
            || relative.contains(['\\', ':', '\0'])
            || Path::new(relative)
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
        {
            return Ok(Self::unresolved("hls-reference-outside-root"));
        }
        let path = parent.parent().unwrap_or(&self.root).join(relative);
        let Ok(path) = path.canonicalize() else {
            return Ok(Self::unresolved("hls-reference-unreadable"));
        };
        if !path.starts_with(&self.root) {
            return Ok(Self::unresolved("hls-reference-outside-root"));
        }
        if self.active.contains(&path) {
            return Ok(Self::unresolved("hls-reference-cycle"));
        }
        if let Some(cached) = self.cache.get(&path) {
            return Ok(cached.clone());
        }
        if self.cache.len() + self.active.len() >= MAX_PLAYLISTS {
            return Err(InspectionError("hls-playlist-limit"));
        }
        if !std::fs::metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            return Ok(Self::unresolved("hls-reference-unreadable"));
        }
        let bytes = read_manifest(&path)?;
        self.bytes += bytes.len() as u64;
        if self.bytes > MAX_TOTAL_BYTES {
            return Err(InspectionError("hls-total-size-limit"));
        }
        self.active.insert(path.clone());
        let result = self.inspect(&path, &bytes)?;
        self.active.remove(&path);
        self.cache.insert(path, result.clone());
        Ok(result)
    }
    fn inspect(&mut self, path: &Path, bytes: &[u8]) -> Result<Inspection, InspectionError> {
        inspect_document(hls_document(bytes)?, |reference| {
            self.child(path, reference)
        })
    }
}

fn inspect_document(
    document: HlsDocument,
    mut child: impl FnMut(&Reference) -> Result<Inspection, InspectionError>,
) -> Result<Inspection, InspectionError> {
    let mut tracks = Vec::new();
    let mut descriptions = Vec::new();
    let mut systems = document.systems;
    let mut findings = document.findings;
    if document.references.is_empty() {
        let mut item = track(Protection::aggregate(document.protections));
        item.key_ids = document.key_ids.into_iter().collect();
        if !document.has_media {
            finding(&mut findings, "hls-no-media-declarations");
        }
        tracks.push(item);
        let mut detail = super::ProtectionDescription::track(&tracks[0], Evidence::Manifest);
        detail.hls_methods(&document.methods);
        descriptions.push(detail);
    } else {
        for reference in document.references {
            let child = child(&reference)?;
            let child_details = child.details.map(|details| details.track_protection);
            for (index, mut item) in child.tracks.into_iter().enumerate() {
                if item.kind == TrackKind::Unknown {
                    item.kind = reference.kind;
                }
                if item.codec.is_none() {
                    item.codec.clone_from(&reference.codec);
                }
                descriptions.push(
                    child_details
                        .as_ref()
                        .and_then(|details| details.get(index))
                        .cloned()
                        .unwrap_or_else(|| {
                            super::ProtectionDescription::track(&item, Evidence::Manifest)
                        }),
                );
                tracks.push(item);
            }
            systems.extend(child.drm_systems);
            for item in child.findings {
                if !findings.iter().any(|known| known == &item) {
                    findings.push(item);
                }
            }
            if tracks.len() > MAX_TRACKS {
                return Err(InspectionError("manifest-track-limit"));
            }
        }
    }
    let mut result = Inspection::new(
        InputFormat::Hls,
        Evidence::Manifest,
        tracks,
        systems,
        findings,
    )
    .with_details(super::PeriodCount::not_applicable());
    result
        .details
        .as_mut()
        .expect("details were added")
        .track_protection = descriptions;
    Ok(result)
}

pub(super) fn inspect_hls_metadata(
    bytes: &[u8],
    child: &mut dyn FnMut(&str) -> Result<Inspection, InspectionError>,
) -> Result<Inspection, InspectionError> {
    Ok(assign_hls_ids(inspect_document(
        hls_document(bytes)?,
        |reference| child(&reference.uri),
    )?))
}
fn assign_hls_ids(mut result: Inspection) -> Inspection {
    for (index, item) in result.tracks.iter_mut().enumerate() {
        item.id = format!("hls-{index:04}");
        if let Some(details) = &mut result.details {
            details.track_protection[index].track_id = Some(item.id.clone());
        }
    }
    result
}

pub(super) fn inspect_hls(path: &Path, bytes: &[u8]) -> Result<Inspection, InspectionError> {
    let root = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|_| InspectionError("input-unreadable"))?;
    let path = path
        .canonicalize()
        .map_err(|_| InspectionError("input-unreadable"))?;
    let mut traversal = Traversal {
        root,
        active: HashSet::from([path.clone()]),
        cache: HashMap::new(),
        references: 0,
        bytes: bytes.len() as u64,
    };
    Ok(assign_hls_ids(traversal.inspect(&path, bytes)?))
}
