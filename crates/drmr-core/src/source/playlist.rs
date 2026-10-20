//! Shared HLS resource planning with explicit encryption admission policies.
//!
//! Accept bounded VOD playlists with one target duration and supported initialization
//! and byte-range forms. Legacy source admission rejects encryption; retained acquisition
//! validates encryption declarations separately. Harmless immediate zero-length cue pairs
//! are handled explicitly, while discontinuities remain unsupported.

use super::fetch::ByteRange;
use super::pytext::{all_digits, ascii_digits, cmp_decimal, decimal_digit, splitlines, strip};
use super::{Failure, FetchPolicy, SourceError, child_url};
use crate::m3u8::{Attributes, parse_attributes};
use std::cmp::Ordering;
use std::path::PathBuf;

/// `_COMMON`: tags of both playlist kinds.
pub const COMMON: [&str; 3] = ["#EXTM3U", "#EXT-X-VERSION", "#EXT-X-INDEPENDENT-SEGMENTS"];
/// `_MEDIA`: media playlist tags.
pub const MEDIA: [&str; 12] = [
    "#EXTINF",
    "#EXT-X-TARGETDURATION",
    "#EXT-X-MEDIA-SEQUENCE",
    "#EXT-X-DISCONTINUITY-SEQUENCE",
    "#EXT-X-PLAYLIST-TYPE",
    "#EXT-X-ENDLIST",
    "#EXT-X-MAP",
    "#EXT-X-BYTERANGE",
    "#EXT-X-KEY",
    "#EXT-X-PROGRAM-DATE-TIME",
    "#EXT-X-CUE-OUT",
    "#EXT-X-CUE-IN",
];
/// `_MASTER`: master playlist tags.
pub const MASTER: [&str; 3] = ["#EXT-X-MEDIA", "#EXT-X-STREAM-INF", "#EXT-X-SESSION-DATA"];

fn token(name: &str) -> SourceError {
    SourceError::new(name)
}

fn fail(name: &str) -> Failure {
    Failure::Source(token(name))
}

/// The part of `line` before its first `:` (`line.split(":", 1)[0]`).
pub fn tag_of(line: &str) -> &str {
    line.split_once(':').map_or(line, |(tag, _)| tag)
}

/// `_lines`: the non-blank lines (stripped) of a UTF-8 playlist (a BOM allowed) that starts
/// with `#EXTM3U` and uses no tag outside the allow-list.
pub fn lines(body: &[u8]) -> Result<Vec<String>, SourceError> {
    let body = body.strip_prefix(b"\xef\xbb\xbf").unwrap_or(body);
    let text = std::str::from_utf8(body).map_err(|_| token("invalid-playlist"))?;
    let lines: Vec<String> = splitlines(text)
        .into_iter()
        .map(strip)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    if lines.first().map(String::as_str) != Some("#EXTM3U") {
        return Err(token("invalid-playlist"));
    }
    for line in &lines {
        if line.starts_with("#EXT") {
            let tag = tag_of(line);
            if !COMMON.contains(&tag) && !MEDIA.contains(&tag) && !MASTER.contains(&tag) {
                return Err(token("unsupported-playlist-tag"));
            }
        }
    }
    Ok(lines)
}

/// `_positive_int`: `[1-9]\d*` (at most 20 characters) as ASCII digits, `invalid-playlist`
/// otherwise.
pub fn positive_int(value: &str) -> Result<String, SourceError> {
    let mut chars = value.chars();
    let valid = value.chars().count() <= 20
        && chars.next().is_some_and(|c| ('1'..='9').contains(&c))
        && chars.all(|c| decimal_digit(c).is_some());
    if !valid {
        return Err(token("invalid-playlist"));
    }
    ascii_digits(value).ok_or_else(|| token("invalid-playlist"))
}

/// `_range(value, previous, url)`: `length[@offset]`; without an offset the range continues
/// the previous one of the same URL. Values beyond `u64` are refused as `invalid-byte-range`.
pub fn byte_range(
    value: &str,
    previous: Option<(&str, u128)>,
    url: &str,
) -> Result<ByteRange, SourceError> {
    let invalid = || token("invalid-byte-range");
    if value.chars().count() > 42 {
        return Err(invalid());
    }
    let (length, offset) = match value.split_once('@') {
        Some((length, offset)) => (length, Some(offset)),
        None => (value, None),
    };
    let mut length_chars = length.chars();
    if !length_chars
        .next()
        .is_some_and(|c| ('1'..='9').contains(&c))
        || !length_chars.all(|c| decimal_digit(c).is_some())
        || offset.is_some_and(|offset| !all_digits(offset))
    {
        return Err(invalid());
    }
    let number = |digits: &str| -> Result<u64, SourceError> {
        ascii_digits(digits)
            .and_then(|ascii| ascii.parse::<u64>().ok())
            .ok_or_else(invalid)
    };
    let offset = match (offset, previous) {
        (Some(offset), _) => number(offset)?,
        (None, Some((previous_url, end))) if previous_url == url => {
            u64::try_from(end).map_err(|_| invalid())?
        }
        _ => return Err(invalid()),
    };
    Ok(ByteRange {
        offset,
        length: number(length)?,
    })
}

/// The kind of media playlist a master names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaylistKind {
    /// An `EXT-X-STREAM-INF` variant.
    Variant,
    /// An `EXT-X-MEDIA` audio rendition with a URI.
    Audio,
    /// An `EXT-X-MEDIA` subtitles rendition.
    Text,
}

/// A media playlist's attributes: the variant's (or rendition's) attribute list and, for a
/// variant whose audio group declares an in-band rendition, that rendition's
/// (`_INBAND_AUDIO`).
#[derive(Clone, Debug, Default)]
pub struct PlaylistAttrs {
    /// The attribute list.
    pub attrs: Attributes,
    /// `_INBAND_AUDIO`.
    pub inband_audio: Option<Attributes>,
}

impl PlaylistAttrs {
    /// `attrs.get(key)`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.attrs.get(key)
    }
}

/// `_master_plan`: the media playlists a master names, in the order the sequential analysis
/// visited them — every variant, then every alternative rendition with a URI.
pub fn master_plan(
    lines: &[String],
    effective: &str,
) -> Result<Vec<(String, PlaylistAttrs, PlaylistKind)>, Failure> {
    let mut variants: Vec<(String, Attributes)> = Vec::new();
    let mut alternatives: Vec<Attributes> = Vec::new();
    let mut pending: Option<Attributes> = None;
    for line in &lines[1..] {
        if let Some(value) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            if pending.is_some() {
                return Err(fail("invalid-playlist"));
            }
            pending = Some(parse_attributes(value).map_err(Failure::Source)?);
        } else if let Some(value) = line.strip_prefix("#EXT-X-MEDIA:") {
            let a = parse_attributes(value).map_err(Failure::Source)?;
            check_rendition(&a)?;
            alternatives.push(a);
        } else if !line.starts_with('#') {
            let Some(a) = pending.take() else {
                return Err(fail("invalid-playlist"));
            };
            if a.get("CLOSED-CAPTIONS").unwrap_or("NONE") != "NONE" {
                return Err(fail("unsupported-source-track"));
            }
            variants.push((child_url(effective, line)?, a));
        } else if let Some(value) = line.strip_prefix("#EXT-X-SESSION-DATA:") {
            let a = parse_attributes(value).map_err(Failure::Source)?;
            if a.get("URI").is_some() {
                return Err(fail("unsupported-playlist-tag"));
            }
        } else if line.starts_with("#EXT") && !COMMON.contains(&tag_of(line)) {
            return Err(fail("ambiguous-playlist"));
        }
    }
    if pending.is_some() || variants.is_empty() {
        return Err(fail("invalid-playlist"));
    }
    let defaults: Vec<(&str, &str)> = alternatives
        .iter()
        .filter(|a| a.get("DEFAULT") == Some("YES"))
        .map(|a| (a.get("TYPE").unwrap_or(""), a.get("GROUP-ID").unwrap_or("")))
        .collect();
    let mut unique = defaults.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != defaults.len() {
        return Err(fail("invalid-playlist"));
    }
    let mut plan = Vec::new();
    for (location, a) in &variants {
        for group in ["AUDIO", "SUBTITLES"] {
            if let Some(name) = a.get(group)
                && !alternatives
                    .iter()
                    .any(|t| t.get("TYPE") == Some(group) && t.get("GROUP-ID") == Some(name))
            {
                return Err(fail("missing-rendition-group"));
            }
        }
        let inband: Vec<&Attributes> = alternatives
            .iter()
            .filter(|t| {
                t.get("TYPE") == Some("AUDIO")
                    && t.get("GROUP-ID") == a.get("AUDIO")
                    && t.get("URI").is_none()
            })
            .collect();
        if inband.len() > 1 {
            return Err(fail("ambiguous-source-topology"));
        }
        let attrs = PlaylistAttrs {
            attrs: a.clone(),
            inband_audio: inband.first().map(|t| (*t).clone()),
        };
        plan.push((location.clone(), attrs, PlaylistKind::Variant));
    }
    for a in &alternatives {
        if let Some(uri) = a.get("URI") {
            let kind = if a.get("TYPE") == Some("AUDIO") {
                PlaylistKind::Audio
            } else {
                PlaylistKind::Text
            };
            let attrs = PlaylistAttrs {
                attrs: a.clone(),
                inband_audio: None,
            };
            plan.push((child_url(effective, uri)?, attrs, kind));
        } else if !variants
            .iter()
            .any(|(_, v)| v.get("AUDIO").is_some() && v.get("AUDIO") == a.get("GROUP-ID"))
        {
            return Err(fail("ambiguous-source-topology"));
        }
    }
    Ok(plan)
}

/// The checks of one `EXT-X-MEDIA` line.
fn check_rendition(a: &Attributes) -> Result<(), Failure> {
    let kind = a.get("TYPE");
    if !matches!(kind, Some("AUDIO" | "SUBTITLES")) {
        return Err(fail("unsupported-source-track"));
    }
    if a.get("GROUP-ID").is_none() || a.get("NAME").is_none() {
        return Err(fail("invalid-playlist"));
    }
    if kind == Some("SUBTITLES") && a.get("URI").is_none() {
        return Err(fail("invalid-playlist"));
    }
    if kind == Some("SUBTITLES") && a.get("FORCED") == Some("YES") {
        return Err(fail("unsupported-forced-subtitles"));
    }
    if !matches!(a.get("FORCED"), None | Some("NO")) {
        return Err(fail("invalid-playlist"));
    }
    if ["DEFAULT", "AUTOSELECT"]
        .iter()
        .any(|flag| !matches!(a.get(flag), None | Some("YES" | "NO")))
    {
        return Err(fail("invalid-playlist"));
    }
    if a.get("DEFAULT") == Some("YES") && a.get("AUTOSELECT") == Some("NO") {
        return Err(fail("invalid-playlist"));
    }
    Ok(())
}

/// A media resource planned from a playlist (`_Resource`): its id, and so its file, is fixed
/// before the download starts.
#[derive(Clone, Debug)]
pub struct Planned {
    /// The resource's file without its suffix (`r000001`).
    pub base: PathBuf,
    /// Where it comes from.
    pub url: String,
    /// The bytes of `url` it is.
    pub byte_range: Option<ByteRange>,
    /// The `EXTINF` duration (segments).
    pub duration: Option<f64>,
    /// An `EXT-X-MAP` was in effect at this segment's line.
    pub has_init: bool,
}

/// A parsed media playlist: its initialization segment and segments.
#[derive(Clone, Debug, Default)]
pub struct MediaPlan {
    /// The `EXT-X-MAP` resource.
    pub init: Option<Planned>,
    /// The segments in playlist order.
    pub segments: Vec<Planned>,
}

/// `_media_plan`: parse a media playlist and plan its resources, allocating their ids in
/// playlist order (`allocate` returns the next resource's base path, or the total-resource
/// budget's refusal).
pub fn media_plan(
    lines: &[String],
    effective: &str,
    policy: &FetchPolicy,
    allocate: &mut dyn FnMut() -> Result<PathBuf, Failure>,
) -> Result<MediaPlan, Failure> {
    media_plan_with_keys(lines, effective, policy, allocate, &mut |value| {
        let a = parse_attributes(value).map_err(Failure::Source)?;
        if a.len() != 1 || a.get("METHOD") != Some("NONE") {
            return Err(fail("encrypted-source-unsupported"));
        }
        Ok(())
    })
}

/// Plan ciphertext acquisition only after the caller validates each key declaration.
/// Key identifiers never become planned resources. Local media admission remains separate.
pub(crate) fn retained_media_plan(
    lines: &[String],
    effective: &str,
    policy: &FetchPolicy,
    allocate: &mut dyn FnMut() -> Result<PathBuf, Failure>,
    validate_key: &mut dyn FnMut(&str) -> Result<(), Failure>,
) -> Result<MediaPlan, Failure> {
    media_plan_with_keys(lines, effective, policy, allocate, validate_key)
}

fn media_plan_with_keys(
    lines: &[String],
    effective: &str,
    policy: &FetchPolicy,
    allocate: &mut dyn FnMut() -> Result<PathBuf, Failure>,
    validate_key: &mut dyn FnMut(&str) -> Result<(), Failure>,
) -> Result<MediaPlan, Failure> {
    if lines.iter().any(|line| MASTER.contains(&tag_of(line))) {
        return Err(fail("ambiguous-playlist"));
    }
    let endlists = lines
        .iter()
        .filter(|line| *line == "#EXT-X-ENDLIST")
        .count();
    if endlists == 0 {
        return Err(fail("live-unsupported"));
    }
    if endlists != 1 {
        return Err(fail("invalid-playlist"));
    }
    let targets: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.strip_prefix("#EXT-X-TARGETDURATION:"))
        .collect();
    if targets.len() != 1 {
        return Err(fail("invalid-target-duration"));
    }
    let target = positive_int(targets[0]).map_err(Failure::Source)?;
    let mut state = MediaState::default();
    let mut plan = MediaPlan::default();
    for index in 1..lines.len() {
        let line = &lines[index];
        if state.ended {
            if !line.starts_with('#') || line.starts_with("#EXT") {
                return Err(fail("invalid-playlist"));
            }
            continue;
        }
        let (tag, value) = line.split_once(':').unwrap_or((line, ""));
        match tag {
            "#EXT-X-ENDLIST" => state.ended = true,
            "#EXT-X-CUE-OUT" => {
                cue_out(lines, index, value, &state, &plan)?;
                state.zero_cue = true;
            }
            "#EXT-X-CUE-IN" => {
                if !state.zero_cue || line != "#EXT-X-CUE-IN" {
                    return Err(fail("unsupported-ad-cue"));
                }
                state.zero_cue = false;
            }
            "#EXTINF" => {
                if state.duration.is_some() {
                    return Err(fail("invalid-playlist"));
                }
                state.duration = Some(extinf_duration(value, &target)?);
            }
            "#EXT-X-KEY" => {
                validate_key(value)?;
            }
            "#EXT-X-PLAYLIST-TYPE" if value != "VOD" => return Err(fail("live-unsupported")),
            "#EXT-X-DISCONTINUITY-SEQUENCE" if value != "0" => {
                return Err(fail("discontinuity-unsupported"));
            }
            "#EXT-X-MAP" => map_line(value, effective, &mut state, &mut plan, allocate)?,
            "#EXT-X-BYTERANGE" => {
                if state.pending_range.is_some() {
                    return Err(fail("invalid-byte-range"));
                }
                state.pending_range = Some(value.to_string());
            }
            _ if !line.starts_with('#') => {
                segment_line(line, effective, policy, &mut state, &mut plan, allocate)?;
            }
            _ => {}
        }
    }
    if state.duration.is_some() || state.pending_range.is_some() || plan.segments.is_empty() {
        return Err(fail("invalid-playlist"));
    }
    Ok(plan)
}

#[derive(Default)]
struct MediaState {
    duration: Option<f64>,
    pending_range: Option<String>,
    previous_range: Option<(String, u128)>,
    init_identity: Option<(String, Option<ByteRange>)>,
    ended: bool,
    zero_cue: bool,
}

/// An immediate zero-length OUT/IN pair at a segment split has no ad
/// interval to map. The original playlist stays frozen; an active break or any segment or
/// timing change in between is refused.
fn cue_out(
    lines: &[String],
    index: usize,
    value: &str,
    state: &MediaState,
    plan: &MediaPlan,
) -> Result<(), Failure> {
    let zero = {
        let (integral, fraction) = value.split_once('.').unwrap_or((value, ""));
        !integral.is_empty()
            && integral.bytes().all(|b| b == b'0')
            && (!value.contains('.')
                || (!fraction.is_empty() && fraction.bytes().all(|b| b == b'0')))
    };
    if state.duration.is_some()
        || state.pending_range.is_some()
        || plan.segments.is_empty()
        || !zero
        || index + 2 >= lines.len()
        || lines[index + 1] != "#EXT-X-CUE-IN"
        || !lines[index + 2].starts_with("#EXTINF:")
    {
        return Err(fail("unsupported-ad-cue"));
    }
    Ok(())
}

/// `#EXTINF:<duration>,…`: a decimal (`\d+(\.\d+)?`, at most 32 characters), positive, below
/// the target duration plus half a second, as the double nearest to it.
fn extinf_duration(value: &str, target: &str) -> Result<f64, Failure> {
    let literal = value.split_once(',').map_or(value, |(literal, _)| literal);
    let (integral, fraction) = match literal.split_once('.') {
        Some((integral, fraction)) => (integral, Some(fraction)),
        None => (literal, None),
    };
    if literal.chars().count() > 32
        || !all_digits(integral)
        || fraction.is_some_and(|fraction| !all_digits(fraction))
    {
        return Err(fail("invalid-duration"));
    }
    let integral = ascii_digits(integral).ok_or_else(|| fail("invalid-duration"))?;
    let fraction = match fraction {
        Some(fraction) => ascii_digits(fraction).ok_or_else(|| fail("invalid-duration"))?,
        None => String::new(),
    };
    if integral.bytes().chain(fraction.bytes()).all(|b| b == b'0') {
        return Err(fail("invalid-duration"));
    }
    let reaches_bound = match cmp_decimal(&integral, target) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => fraction
            .as_bytes()
            .first()
            .is_some_and(|digit| *digit >= b'5'),
    };
    if reaches_bound {
        return Err(fail("invalid-target-duration"));
    }
    let text = if fraction.is_empty() {
        integral
    } else {
        format!("{integral}.{fraction}")
    };
    text.parse::<f64>().map_err(|_| fail("invalid-duration"))
}

fn map_line(
    value: &str,
    effective: &str,
    state: &mut MediaState,
    plan: &mut MediaPlan,
    allocate: &mut dyn FnMut() -> Result<PathBuf, Failure>,
) -> Result<(), Failure> {
    let a = parse_attributes(value).map_err(Failure::Source)?;
    let Some(uri) = a.get("URI") else {
        return Err(fail("invalid-playlist"));
    };
    let location = child_url(effective, uri)?;
    let range = match a.get("BYTERANGE") {
        Some(range) => Some(byte_range(range, None, &location).map_err(Failure::Source)?),
        None => None,
    };
    let identity = (location.clone(), range);
    if let Some(existing) = &state.init_identity
        && *existing != identity
    {
        return Err(fail("changing-initialization-unsupported"));
    }
    if plan.init.is_none() {
        plan.init = Some(Planned {
            base: allocate()?,
            url: location,
            byte_range: range,
            duration: None,
            has_init: false,
        });
        state.init_identity = Some(identity);
    }
    Ok(())
}

fn segment_line(
    line: &str,
    effective: &str,
    policy: &FetchPolicy,
    state: &mut MediaState,
    plan: &mut MediaPlan,
    allocate: &mut dyn FnMut() -> Result<PathBuf, Failure>,
) -> Result<(), Failure> {
    let Some(duration) = state.duration else {
        return Err(fail("invalid-playlist"));
    };
    if plan.segments.len() as u64 >= policy.max_segments_per_track {
        return Err(fail("segment-budget-exceeded"));
    }
    let location = child_url(effective, line)?;
    // `_range(...) if pending_range else None`: an empty value (`#EXT-X-BYTERANGE:` or the bare
    // tag) is falsy, so the segment is fetched whole.
    let range = match &state.pending_range {
        Some(value) if !value.is_empty() => Some(
            byte_range(
                value,
                state
                    .previous_range
                    .as_ref()
                    .map(|(url, end)| (url.as_str(), *end)),
                &location,
            )
            .map_err(Failure::Source)?,
        ),
        _ => None,
    };
    plan.segments.push(Planned {
        base: allocate()?,
        url: location.clone(),
        byte_range: range,
        duration: Some(duration),
        has_init: plan.init.is_some(),
    });
    state.previous_range = range.map(|range| {
        (
            location,
            u128::from(range.offset) + u128::from(range.length),
        )
    });
    state.duration = None;
    state.pending_range = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_comments_preserve_the_pending_variant_uri() {
        let body = b"#EXTM3U\n## Generated by a packager\n# ordinary comment\n#EXT-X-STREAM-INF:BANDWIDTH=128000\n# comment before the URI\n## another comment\nvideo/media.m3u8\n# trailing comment\n";
        let parsed = lines(body).unwrap();
        let plan = master_plan(&parsed, "https://media.example.invalid/master.m3u8").unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].0, "https://media.example.invalid/video/media.m3u8");
        assert_eq!(plan[0].2, PlaylistKind::Variant);
        assert_eq!(plan[0].1.get("BANDWIDTH"), Some("128000"));
    }

    #[test]
    fn comments_do_not_allow_unknown_ext_tags_or_media_tags_in_a_master() {
        for tag in ["#EXT-X-UNKNOWN:1", "#EXTUNKNOWN:1"] {
            let body = format!(
                "#EXTM3U\n# comment\n{tag}\n#EXT-X-STREAM-INF:BANDWIDTH=128000\nvideo.m3u8\n"
            );
            assert_eq!(
                lines(body.as_bytes()).unwrap_err().token(),
                "unsupported-playlist-tag"
            );
        }
        let parsed = lines(b"#EXTM3U\n# comment\n#EXT-X-TARGETDURATION:2\n#EXT-X-STREAM-INF:BANDWIDTH=128000\nvideo.m3u8\n").unwrap();
        assert_eq!(
            master_plan(&parsed, "https://media.example.invalid/master.m3u8")
                .unwrap_err()
                .token(),
            Some("ambiguous-playlist")
        );
    }

    #[test]
    fn media_comments_leave_segment_and_endlist_rules_intact() {
        let body = b"#EXTM3U\n## Generated by a packager\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\n# comment before the segment\nsegment.ts\n#EXT-X-ENDLIST\n# trailing comment\n";
        let parsed = lines(body).unwrap();
        let mut allocate = || Ok(PathBuf::from("segment"));
        let plan = media_plan(
            &parsed,
            "https://media.example.invalid/media.m3u8",
            &FetchPolicy::default(),
            &mut allocate,
        )
        .unwrap();
        assert_eq!(plan.segments.len(), 1);
        assert_eq!(plan.segments[0].duration, Some(2.0));
        let parsed = lines(&[body.as_slice(), b"#EXT-X-VERSION:7\n"].concat()).unwrap();
        assert_eq!(
            media_plan(
                &parsed,
                "https://media.example.invalid/media.m3u8",
                &FetchPolicy::default(),
                &mut allocate
            )
            .unwrap_err()
            .token(),
            Some("invalid-playlist")
        );
    }
}
