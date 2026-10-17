//! Generate DASH and HLS manifests for packaged clear-input presentations.
//!
//! Tracks share an exact rational presentation timeline. Encrypted tracks carry
//! Widevine, `PlayReady`, and `FairPlay` signaling. `FairPlay` URIs and IVs never appear in
//! debug output.

use crate::manifest_support::fraction::Fraction;
use crate::manifest_support::path;
use crate::manifest_support::py::{self, PyErr, format_fixed};
use crate::manifest_support::xml::{self, Element, NamespaceMap};
use crate::playready::PlayReadyObjects;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use num_bigint::BigInt;
use num_traits::Zero;
use std::fmt;
use std::path::{Path, PathBuf};

const MPD: &str = "urn:mpeg:dash:schema:mpd:2011";
const CENC: &str = "urn:mpeg:cenc:2013";
const MSPR: &str = "urn:microsoft:playready";
const WV: &str = "edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";
const PR: &str = "9a04f079-9840-4286-ab92-e65be0885f95";
const DESCRIBES_VIDEO: &str = "public.accessibility.describes-video";
const CAPTIONS: [&str; 2] = [
    "public.accessibility.describes-music-and-sound",
    "public.accessibility.transcribes-spoken-dialog",
];

/// `OutputSegment`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputSegment {
    /// The source segment's index.
    pub source_index: i64,
    /// The written segment.
    pub path: PathBuf,
    /// Ticks per second.
    pub timescale: i64,
    /// The first decode time, in ticks.
    pub decode_start: i64,
    /// The duration, in ticks.
    pub duration: i64,
    /// The first presentation time, in ticks.
    pub presentation_start: i64,
    /// Samples in the segment.
    pub sample_count: i64,
}

/// `OutputTrack` (the Python defaults are [`OutputTrack::new`]'s).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputTrack {
    /// The track id (the media playlist's file stem).
    pub id: String,
    /// `video` | `audio` | `text`.
    pub content_type: String,
    /// The codec string (`wvtt` for `WebVTT`).
    pub codec: String,
    /// The segments, in order.
    pub segments: Vec<OutputSegment>,
    /// The init segment (`None` for a track without one).
    pub init_path: Option<PathBuf>,
    /// Bits per second.
    pub bandwidth: i64,
    /// Default `und`.
    pub language: String,
    /// Default `main`.
    pub role: String,
    /// Default empty.
    pub name: String,
    /// Default empty.
    pub group_id: String,
    /// Default 0.
    pub width: i64,
    /// Default 0.
    pub height: i64,
    /// Default empty.
    pub frame_rate: String,
    /// Default `true`.
    pub encrypted: bool,
    /// Default empty.
    pub audio_group: String,
    /// Default empty.
    pub subtitle_group: String,
    /// Default `None`.
    pub text_map_offset: Option<Fraction>,
    /// Default `None`.
    pub dash_presentation_time_offset: Option<Fraction>,
    /// Default `None`.
    pub text_earliest_cue: Option<Fraction>,
    /// Default `false`.
    pub is_default: bool,
    /// Default `false`.
    pub autoselect: bool,
}

impl OutputTrack {
    /// `OutputTrack(id, content_type, codec, segments, init_path, bandwidth)` with Python's
    /// defaults for every other field.
    pub fn new(
        id: &str,
        content_type: &str,
        codec: &str,
        segments: Vec<OutputSegment>,
        init_path: Option<PathBuf>,
        bandwidth: i64,
    ) -> Self {
        OutputTrack {
            id: id.to_string(),
            content_type: content_type.to_string(),
            codec: codec.to_string(),
            segments,
            init_path,
            bandwidth,
            language: "und".to_string(),
            role: "main".to_string(),
            name: String::new(),
            group_id: String::new(),
            width: 0,
            height: 0,
            frame_rate: String::new(),
            encrypted: true,
            audio_group: String::new(),
            subtitle_group: String::new(),
            text_map_offset: None,
            dash_presentation_time_offset: None,
            text_earliest_cue: None,
            is_default: false,
            autoselect: false,
        }
    }
}

/// `PackagedPresentation`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackagedPresentation {
    /// The tracks, in output order.
    pub tracks: Vec<OutputTrack>,
    /// The KID (public).
    pub key_id_hex: String,
    /// The Widevine `pssh` box.
    pub widevine_pssh: Vec<u8>,
    /// The `PlayReady` objects ( single source).
    pub playready: PlayReadyObjects,
    /// The common presentation origin, in seconds.
    pub origin: Fraction,
}

/// `PrivateFairPlaySignaling`: both fields are `repr=False` in Python; `Debug` shows neither.
#[derive(Clone, PartialEq, Eq)]
pub struct PrivateFairPlaySignaling {
    /// The `skd://` URI (secret in logs).
    pub uri: String,
    /// The IV (secret in logs).
    pub iv_hex: String,
}

impl fmt::Debug for PrivateFairPlaySignaling {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrivateFairPlaySignaling")
            .finish_non_exhaustive()
    }
}

fn value_error(token: &str) -> PyErr {
    PyErr::Value(token.to_string())
}

/// `_relative(path, destination)`: `path` relative to `destination`'s directory, POSIX form.
fn relative(target: &Path, destination: &Path) -> Result<String, PyErr> {
    let directory = path::parent(&path::fspath(destination)?);
    Ok(path::pure(&path::relpath(
        &path::fspath(target)?,
        &directory,
    )?))
}

/// `_quote(value)`: a quoted HLS attribute value; `"`, CR, LF and NUL are refused.
fn quote(value: &str) -> Result<String, PyErr> {
    if value.contains(['"', '\r', '\n', '\0']) {
        return Err(value_error("invalid-manifest-attribute"));
    }
    Ok(format!("\"{value}\""))
}

/// `_seconds(value)`: `f"{float(value):.9f}"` without trailing zeros (and point).
fn seconds(value: &Fraction) -> Result<String, PyErr> {
    let fixed = format_fixed(value.to_f64()?, 9);
    let trimmed = fixed.trim_end_matches('0').trim_end_matches('.');
    Ok(if trimmed.is_empty() {
        "0".to_string()
    } else {
        trimmed.to_string()
    })
}

/// `_role_metadata`: `(DASH role, DASH accessibility, HLS characteristics)`; unknown semantics
/// are `unsupported-rendition-role`.
fn role_metadata(track: &OutputTrack) -> Result<(String, Option<String>, Option<String>), PyErr> {
    let role = if track.role.is_empty() {
        if track.content_type == "text" {
            "subtitle"
        } else {
            "main"
        }
    } else {
        track.role.as_str()
    };
    if role == "main" || role == "subtitle" {
        return Ok((role.to_string(), None, None));
    }
    if role == "description" && track.content_type == "audio" {
        return Ok((
            role.to_string(),
            Some("description".to_string()),
            Some(DESCRIBES_VIDEO.to_string()),
        ));
    }
    if role == "caption" && track.content_type == "text" {
        return Ok((
            role.to_string(),
            Some("caption".to_string()),
            Some(CAPTIONS.join(",")),
        ));
    }
    let values: Vec<&str> = role.split(',').collect();
    let mut unique = values.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != values.len() {
        return Err(value_error("unsupported-rendition-role"));
    }
    if unique == [DESCRIBES_VIDEO] && track.content_type == "audio" {
        return Ok((
            "main".to_string(),
            Some("description".to_string()),
            Some(role.to_string()),
        ));
    }
    if values.iter().all(|value| CAPTIONS.contains(value)) && track.content_type == "text" {
        return Ok((
            "subtitle".to_string(),
            Some("caption".to_string()),
            Some(role.to_string()),
        ));
    }
    Err(value_error("unsupported-rendition-role"))
}

/// `str(uuid.UUID(hex=value))`.
fn uuid_string(value: &str) -> Result<String, PyErr> {
    let stripped = value.replace("urn:", "").replace("uuid:", "");
    let stripped = stripped.trim_matches(['{', '}']).replace('-', "");
    if stripped.chars().count() != 32 {
        return Err(value_error("badly formed hexadecimal UUID string"));
    }
    let number = py::parse_int_radix(&stripped, 16)?;
    let limit = BigInt::from(1u8) << 128;
    if number < BigInt::zero() || number >= limit {
        return Err(value_error("int is out of range (need a 128-bit value)"));
    }
    let hex = format!("{number:032x}");
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// The `_namespace_map` `write_dash` serializes with: `ElementTree`'s defaults after
/// `register_namespace("", MPD)`, `("cenc", CENC)`, `("mspr", MSPR)`.
fn namespace_map() -> NamespaceMap {
    let mut map = NamespaceMap::elementtree_default();
    for (prefix, namespace) in [("", MPD), ("cenc", CENC), ("mspr", MSPR)] {
        map.register(prefix, namespace)
            .expect("the prefixes are not reserved");
    }
    map
}

/// `max(…)` of an empty iterable, as `CPython` 3.11 words it.
fn empty_max() -> PyErr {
    value_error("max() arg is an empty sequence")
}

/// `Fraction(numerator, denominator)` of two Python ints.
fn ratio(numerator: BigInt, denominator: i64) -> Result<Fraction, PyErr> {
    Fraction::new(numerator, denominator)
}

/// `write_dash`: the MPD for `presentation` at `destination`; returns `destination`.
pub fn write_dash(
    presentation: &PackagedPresentation,
    destination: &Path,
) -> Result<PathBuf, PyErr> {
    py::mkdir_parents(Path::new(&path::parent(&path::fspath(destination)?)))?;
    let mut end: Option<Fraction> = None;
    for segment in presentation.tracks.iter().flat_map(|track| &track.segments) {
        let value = ratio(
            BigInt::from(segment.presentation_start) + segment.duration,
            segment.timescale,
        )?;
        if end.as_ref().is_none_or(|current| value > *current) {
            end = Some(value);
        }
    }
    let end = end.ok_or_else(empty_max)?;
    let duration = format!("PT{}S", seconds(&(&end - &presentation.origin))?);
    let mut root = Element::new(
        &format!("{{{MPD}}}MPD"),
        &[
            ("type", "static"),
            ("profiles", "urn:mpeg:dash:profile:full:2011"),
            ("mediaPresentationDuration", &duration),
            ("minBufferTime", "PT2S"),
        ],
    );
    let period = root.sub_element("Period", &[("id", "0"), ("start", "PT0S")]);
    for (number, track) in presentation.tracks.iter().enumerate() {
        dash_adaptation_set(period, number, track, presentation, destination)?;
    }
    xml::indent(&mut root);
    let bytes = xml::tostring_with_declaration(&root, &namespace_map());
    py::write_bytes(destination, bytes.as_bytes())?;
    Ok(destination.to_path_buf())
}

fn dash_adaptation_set(
    period: &mut Element,
    number: usize,
    track: &OutputTrack,
    presentation: &PackagedPresentation,
    destination: &Path,
) -> Result<(), PyErr> {
    let mime = if track.content_type == "text" {
        if track.codec == "wvtt" {
            "text/vtt".to_string()
        } else {
            "application/mp4".to_string()
        }
    } else {
        format!("{}/mp4", track.content_type)
    };
    let number = number.to_string();
    let adaptation_set = period.sub_element(
        "AdaptationSet",
        &[
            ("id", &number),
            ("contentType", &track.content_type),
            ("lang", &track.language),
        ],
    );
    if track.encrypted {
        let kid = uuid_string(&presentation.key_id_hex)?;
        adaptation_set.sub_element(
            "ContentProtection",
            &[
                ("schemeIdUri", "urn:mpeg:dash:mp4protection:2011"),
                ("value", "cbcs"),
                (&format!("{{{CENC}}}default_KID"), &kid),
            ],
        );
        for (system, pssh) in [
            (WV, &presentation.widevine_pssh),
            (PR, &presentation.playready.pssh_box),
        ] {
            let scheme = format!("urn:uuid:{system}");
            let cp = adaptation_set.sub_element("ContentProtection", &[("schemeIdUri", &scheme)]);
            cp.sub_element(&format!("{{{CENC}}}pssh"), &[]).text = Some(STANDARD.encode(pssh));
            if system == PR {
                cp.sub_element(&format!("{{{MSPR}}}pro"), &[]).text =
                    Some(presentation.playready.pro_b64.clone());
            }
        }
    }
    let (role, accessibility, _) = role_metadata(track)?;
    if let Some(accessibility) = accessibility {
        adaptation_set.sub_element(
            "Accessibility",
            &[
                ("schemeIdUri", "urn:mpeg:dash:role:2011"),
                ("value", &accessibility),
            ],
        );
    }
    adaptation_set.sub_element(
        "Role",
        &[("schemeIdUri", "urn:mpeg:dash:role:2011"), ("value", &role)],
    );
    let bandwidth = track.bandwidth.max(1).to_string();
    let mut attrs: Vec<(&str, String)> = vec![
        ("id", track.id.clone()),
        ("bandwidth", bandwidth),
        ("mimeType", mime),
    ];
    if track.codec != "wvtt" {
        attrs.push(("codecs", track.codec.clone()));
    }
    if track.width != 0 {
        attrs.push(("width", track.width.to_string()));
        attrs.push(("height", track.height.to_string()));
    }
    if !track.frame_rate.is_empty() {
        attrs.push(("frameRate", track.frame_rate.clone()));
    }
    let attrs: Vec<(&str, &str)> = attrs.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let representation = adaptation_set.sub_element("Representation", &attrs);
    dash_segment_list(representation, track, presentation, destination)
}

fn dash_segment_list(
    representation: &mut Element,
    track: &OutputTrack,
    presentation: &PackagedPresentation,
    destination: &Path,
) -> Result<(), PyErr> {
    let Some(first) = track.segments.first() else {
        return Err(PyErr::Index("tuple index out of range".to_string()));
    };
    let scale = BigInt::from(first.timescale);
    let override_offset = track.dash_presentation_time_offset.as_ref();
    let pto = override_offset.unwrap_or(&presentation.origin) * &scale;
    if !pto.is_integer() || pto.sign_cmp().is_lt() {
        return Err(value_error("nonintegral-presentation-origin"));
    }
    let pto = pto.trunc();
    let listing = representation.sub_element(
        "SegmentList",
        &[
            ("timescale", &scale.to_string()),
            ("presentationTimeOffset", &pto.to_string()),
            ("startNumber", "0"),
        ],
    );
    if let Some(init_path) = &track.init_path {
        let source_url = relative(init_path, destination)?;
        listing.sub_element("Initialization", &[("sourceURL", &source_url)]);
    }
    let origin_ticks = (&presentation.origin * &scale).trunc();
    let timeline = listing.sub_element("SegmentTimeline", &[]);
    for segment in &track.segments {
        let mut start = BigInt::from(segment.presentation_start);
        if override_offset.is_some() {
            start = start - &origin_ticks + &pto;
        }
        timeline.sub_element(
            "S",
            &[
                ("t", &start.to_string()),
                ("d", &segment.duration.to_string()),
            ],
        );
    }
    for segment in &track.segments {
        let media = relative(&segment.path, destination)?;
        listing.sub_element("SegmentURL", &[("media", &media)]);
    }
    Ok(())
}

/// The group a track belongs to in the master (`audio` / `subtitles` by default).
fn default_group(track: &OutputTrack) -> &str {
    if !track.group_id.is_empty() {
        &track.group_id
    } else if track.content_type == "audio" {
        "audio"
    } else {
        "subtitles"
    }
}

/// `write_hls`: the master at `destination` and one media playlist per track beside it;
/// returns `(destination, *media_paths)`. The `FairPlay` URI and IV are written into the
/// playlists (they are signaling), never into an error.
pub fn write_hls(
    presentation: &PackagedPresentation,
    destination: &Path,
    fairplay: &PrivateFairPlaySignaling,
) -> Result<Vec<PathBuf>, PyErr> {
    let directory = PathBuf::from(path::parent(&path::fspath(destination)?));
    py::mkdir_parents(&directory)?;
    let audio: Vec<&OutputTrack> = presentation
        .tracks
        .iter()
        .filter(|t| t.content_type == "audio")
        .collect();
    let text: Vec<&OutputTrack> = presentation
        .tracks
        .iter()
        .filter(|t| t.content_type == "text")
        .collect();
    let mut groups: Vec<(String, Vec<&OutputTrack>)> = Vec::new();
    for track in &audio {
        let group = if track.group_id.is_empty() {
            "audio"
        } else {
            &track.group_id
        };
        match groups.iter_mut().find(|(known, _)| known == group) {
            Some((_, members)) => members.push(track),
            None => groups.push((group.to_string(), vec![track])),
        }
    }
    let mut defaults: Vec<(&str, &str)> = Vec::new();
    for track in audio.iter().chain(&text) {
        if track.is_default {
            let identity = (track.content_type.as_str(), default_group(track));
            if defaults.contains(&identity) {
                return Err(value_error("invalid-rendition-selection-flags"));
            }
            defaults.push(identity);
        }
    }
    let mut master = vec!["#EXTM3U".to_string(), "#EXT-X-VERSION:7".to_string()];
    let mut media_paths = Vec::new();
    for track in &presentation.tracks {
        let media = directory.join(format!("{}.m3u8", track.id));
        media_paths.push(media.clone());
        write_media_playlist(presentation, track, &media, fairplay)?;
        if track.content_type == "audio" || track.content_type == "text" {
            master.push(media_line(track, &media)?);
        }
    }
    let videos: Vec<&OutputTrack> = presentation
        .tracks
        .iter()
        .filter(|t| t.content_type == "video")
        .collect();
    let variants = if videos.is_empty() { &audio } else { &videos };
    for track in variants {
        master.extend(stream_inf_lines(track, &groups, !videos.is_empty(), &text)?);
    }
    py::write_bytes(destination, format!("{}\n", master.join("\n")).as_bytes())?;
    let mut paths = vec![destination.to_path_buf()];
    paths.extend(media_paths);
    Ok(paths)
}

fn write_media_playlist(
    presentation: &PackagedPresentation,
    track: &OutputTrack,
    media: &Path,
    fairplay: &PrivateFairPlaySignaling,
) -> Result<(), PyErr> {
    let durations: Vec<Fraction> = track
        .segments
        .iter()
        .map(|segment| ratio(BigInt::from(segment.duration), segment.timescale))
        .collect::<Result<_, _>>()?;
    let longest = durations.iter().max().ok_or_else(empty_max)?;
    let target = longest.ceil().max(BigInt::from(1u8));
    let mut lines = vec![
        "#EXTM3U".to_string(),
        "#EXT-X-VERSION:7".to_string(),
        format!("#EXT-X-TARGETDURATION:{target}"),
        "#EXT-X-MEDIA-SEQUENCE:0".to_string(),
        "#EXT-X-PLAYLIST-TYPE:VOD".to_string(),
    ];
    if let Some(init_path) = &track.init_path {
        lines.push(format!(
            "#EXT-X-MAP:URI={}",
            quote(&relative(init_path, media)?)?
        ));
    }
    if track.encrypted {
        let tail = format!(",IV=0x{},KEYFORMATVERSIONS=\"1\"", fairplay.iv_hex);
        let widevine = STANDARD.encode(&presentation.widevine_pssh);
        lines.push(format!(
            "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"data:text/plain;base64,{widevine}\",KEYID=0x{}{tail},KEYFORMAT=\"urn:uuid:{WV}\"",
            presentation.key_id_hex
        ));
        lines.push(format!(
            "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"data:text/plain;charset=UTF-16;base64,{}\"{tail},KEYFORMAT=\"com.microsoft.playready\"",
            presentation.playready.pro_b64
        ));
        lines.push(format!(
            "#EXT-X-KEY:METHOD=SAMPLE-AES,URI={}{tail},KEYFORMAT=\"com.apple.streamingkeydelivery\"",
            quote(&fairplay.uri)?
        ));
    }
    for (segment, duration) in track.segments.iter().zip(&durations) {
        lines.push(format!("#EXTINF:{},", seconds(duration)?));
        lines.push(relative(&segment.path, media)?);
    }
    lines.push("#EXT-X-ENDLIST".to_string());
    py::write_bytes(media, format!("{}\n", lines.join("\n")).as_bytes())
}

fn media_line(track: &OutputTrack, media: &Path) -> Result<String, PyErr> {
    let group = default_group(track);
    let kind = if track.content_type == "audio" {
        "AUDIO"
    } else {
        "SUBTITLES"
    };
    if track.is_default && !track.autoselect {
        return Err(value_error("invalid-rendition-selection-flags"));
    }
    let default = if track.is_default { "YES" } else { "NO" };
    let autoselect = if track.autoselect { "YES" } else { "NO" };
    let (_, _, characteristics) = role_metadata(track)?;
    let name = if track.name.is_empty() {
        &track.id
    } else {
        &track.name
    };
    let mut attributes = format!(
        "#EXT-X-MEDIA:TYPE={kind},GROUP-ID={},NAME={},LANGUAGE={},DEFAULT={default},AUTOSELECT={autoselect},URI={}",
        quote(group)?,
        quote(name)?,
        quote(&track.language)?,
        quote(path::name(&path::fspath(media)?))?
    );
    if let Some(characteristics) = characteristics {
        attributes.push_str(",CHARACTERISTICS=");
        attributes.push_str(&quote(&characteristics)?);
    }
    Ok(attributes)
}

fn stream_inf_lines(
    track: &OutputTrack,
    groups: &[(String, Vec<&OutputTrack>)],
    has_videos: bool,
    text: &[&OutputTrack],
) -> Result<Vec<String>, PyErr> {
    let mut associations: Vec<(String, Vec<&OutputTrack>)> = if has_videos && !groups.is_empty() {
        groups.to_vec()
    } else {
        vec![(String::new(), Vec::new())]
    };
    if !track.audio_group.is_empty() {
        let Some((_, members)) = groups.iter().find(|(name, _)| *name == track.audio_group) else {
            return Err(value_error("missing-output-audio-group"));
        };
        associations = vec![(track.audio_group.clone(), members.clone())];
    }
    let mut lines = Vec::new();
    for (group, members) in associations {
        let mut codecs: Vec<&str> = vec![&track.codec];
        for member in &members {
            if !codecs.contains(&member.codec.as_str()) {
                codecs.push(&member.codec);
            }
        }
        let loudest = members
            .iter()
            .map(|member| member.bandwidth)
            .max()
            .unwrap_or(0);
        let bandwidth = (track.bandwidth + loudest).max(1);
        let mut attrs = vec![
            format!("BANDWIDTH={bandwidth}"),
            format!("CODECS={}", quote(&codecs.join(","))?),
        ];
        if track.width != 0 {
            attrs.push(format!("RESOLUTION={}x{}", track.width, track.height));
        }
        if !track.frame_rate.is_empty() {
            let rate = Fraction::parse(&track.frame_rate)?.to_f64()?;
            attrs.push(format!("FRAME-RATE={}", format_fixed(rate, 3)));
        }
        if !group.is_empty() {
            attrs.push(format!("AUDIO={}", quote(&group)?));
        }
        if !text.is_empty() {
            let mut subtitle_groups: Vec<&str> = text.iter().map(|t| default_group(t)).collect();
            subtitle_groups.sort_unstable();
            subtitle_groups.dedup();
            let subtitle_group = if !track.subtitle_group.is_empty() {
                track.subtitle_group.as_str()
            } else if subtitle_groups.len() == 1 {
                subtitle_groups[0]
            } else {
                ""
            };
            if !subtitle_group.is_empty() {
                if !subtitle_groups.contains(&subtitle_group) {
                    return Err(value_error("missing-output-subtitle-group"));
                }
                attrs.push(format!("SUBTITLES={}", quote(subtitle_group)?));
            }
        }
        lines.push(format!("#EXT-X-STREAM-INF:{}", attrs.join(",")));
        lines.push(format!("{}.m3u8", track.id));
    }
    Ok(lines)
}
