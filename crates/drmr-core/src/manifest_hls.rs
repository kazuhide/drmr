//! Patch HLS playlists to reference a shared CBCS media presentation.
//!
//! Consolidate paired audio groups, preserve master versions and playlist addresses,
//! and update media maps and segment references. Key lines remain verbatim and their
//! IVs are checked. Match playlists to DASH renditions using codec, bandwidth rank,
//! resolution, frame rate, and language; reject ambiguous or conflicting matches.
//! Timing and decoded-content checks verify the resulting media.

use crate::checks::CheckId;
use crate::config::{self, KeyMaterial, Rendition};
use crate::manifest_support::fraction::Fraction;
use crate::manifest_support::path;
use crate::manifest_support::py::{self, PyErr, float_str, format_fixed, repr_str};
use num_bigint::BigInt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// `config::find_download_root`, its exceptions as this module raises them.
fn find_download_root(path: &Path) -> Result<PathBuf, PyErr> {
    Ok(config::find_download_root(path)?)
}

/// `config::resolve_local_path`, its exceptions as this module raises them.
fn resolve_local_path(
    download_root: &Path,
    base_url: &str,
    relative_url: &str,
) -> Result<PathBuf, PyErr> {
    Ok(config::resolve_local_path(
        download_root,
        base_url,
        relative_url,
    )?)
}

/// The EXTINF tolerance (`_EXTINF_TOLERANCE_S`): 5 ms, under one AAC frame (21 ms).
pub const EXTINF_TOLERANCE_S: f64 = 0.005;

const UNMODELED_TAG_PREFIXES: [&str; 3] = [
    "#EXT-X-I-FRAME-STREAM-INF",
    "#EXT-X-SESSION-KEY",
    "#EXT-X-SESSION-DATA",
];
const UNMODELED_MEDIA_TYPES: [&str; 2] = ["SUBTITLES", "CLOSED-CAPTIONS"];
const WAIVED_TS_H264: &str = "waived:TS-H264";

/// `_ATTR_RE`: `KEY=VALUE` pairs, a value quoted (commas allowed inside) or an unquoted run to
/// the next comma. The `regex` crate's leftmost-first matching is Python's for this pattern.
static ATTR_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r#"([A-Z0-9-]+)=("[^"]*"|[^,]*)"#).expect("static pattern"));

/// `HlsRewriteError`: malformed / unmodeled input, a mapping disagreement or ambiguity, an IV
/// mismatch (FAIL-loud; a Python `ValueError`). Never carries key material or the IV value.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HlsRewriteError(pub String);

/// Every exception the functions can raise, by class; `Display` is `str(exc)`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum HlsError {
    /// `HlsRewriteError`.
    #[error(transparent)]
    Rewrite(#[from] HlsRewriteError),
    /// A built-in exception: config's `ValueError` (download root, confinement), `OSError`,
    /// `UnicodeDecodeError`, `int(float)`'s `ValueError` / `OverflowError`, the media tools'
    /// `RuntimeError`.
    #[error(transparent)]
    Py(#[from] PyErr),
}

impl HlsError {
    /// Whether Python's `except ValueError` catches it.
    pub fn is_value_error(&self) -> bool {
        match self {
            HlsError::Rewrite(_) => true,
            HlsError::Py(err) => err.is_value_error(),
        }
    }
}

fn rewrite_error<T>(message: impl Into<String>) -> Result<T, HlsError> {
    Err(HlsError::Rewrite(HlsRewriteError(message.into())))
}

/// `HlsVariantMapping`: one surviving HLS playlist → its DASH rendition, with the evidence
/// (`signals`, insertion-ordered as Python's dict) and the content-gate label.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlsVariantMapping {
    /// The source media playlist (resolved).
    pub hls_playlist: PathBuf,
    /// The DASH rendition it maps to.
    pub dash_rendition: Rendition,
    /// The agreeing signals (public values only).
    pub signals: Vec<(String, String)>,
    /// `framemd5-eq` | `pcm-eq` | `waived:TS-H264`.
    pub content_gate: String,
}

impl HlsVariantMapping {
    /// `signals.get(key)`.
    pub fn signal(&self, key: &str) -> Option<&str> {
        self.signals
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
}

/// `HlsRewriteResult`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlsRewriteResult {
    /// The written master.
    pub output_master_path: PathBuf,
    /// The written media playlists, in mapping order.
    pub output_media_playlist_paths: Vec<PathBuf>,
    /// `AUDIO=` remaps (`ts_AUDIO-N` → `fmp4_AUDIO-N`).
    pub audio_groups_consolidated: usize,
    /// `ts_AUDIO-*` `#EXT-X-MEDIA` lines dropped.
    pub ts_audio_groups_removed: usize,
    /// Formerly-TS playlists that gained an `#EXT-X-MAP`.
    pub ext_x_map_added: usize,
    /// Segment URLs repointed.
    pub segment_refs_repointed: usize,
    /// Every playlist's `EXT-X-KEY` IV verified.
    pub iv_verified: bool,
    /// The mapping.
    pub variant_mapping: Vec<HlsVariantMapping>,
    /// Check id → `PASS` | `FAIL: …` | `SKIP: …`, in Python's insertion order.
    pub checks: Vec<(CheckId, String)>,
}

/// The `media_inspect` functions the gates call, one method per function with its
/// arguments (the tool binaries are the engine's concern). `RuntimeError` is
/// [`PyErr::Runtime`]; the gates catch it (and only it) at the corresponding call site.
pub trait GateMedia {
    /// `compute_framemd5(path)`: ffmpeg's `-f framemd5` report text.
    fn compute_framemd5(&self, path: &Path) -> Result<String, PyErr>;
    /// `decode_hls_media_playlist(localized_playlist_path, output_path, mode=mode)` (`pcm` |
    /// `framemd5`); returns `output_path`.
    fn decode_hls_media_playlist(
        &self,
        localized_playlist_path: &Path,
        output_path: &Path,
        mode: &str,
    ) -> Result<PathBuf, PyErr>;
    /// `decode_media_to_pcm(input_path, output_path)`; returns `output_path`.
    fn decode_media_to_pcm(&self, input_path: &Path, output_path: &Path) -> Result<PathBuf, PyErr>;
    /// `localize_hls_media_playlist(playlist_text, key_file_path=…, resolve_uri=…)`.
    fn localize_hls_media_playlist(
        &self,
        playlist_text: &str,
        key_file_path: &Path,
        resolve_uri: &dyn Fn(&str) -> Result<PathBuf, PyErr>,
    ) -> Result<String, PyErr>;
    /// `measure_segment_durations_s(init_path, segment_paths, timescale)`.
    fn measure_segment_durations_s(
        &self,
        init_path: &Path,
        segment_paths: &[PathBuf],
        timescale: i64,
    ) -> Result<Vec<f64>, PyErr>;
    /// `write_raw_key_file(key_hex, dest)`; returns `dest`.
    fn write_raw_key_file(&self, key_hex: &str, dest: &Path) -> Result<PathBuf, PyErr>;
}

// ------------------------------------------------------------------------------------------
// Line and attribute helpers (locate-only)
// ------------------------------------------------------------------------------------------

/// `_parse_attrs`: the attribute list after the first `:`, quotes stripped from a quoted value;
/// a repeated key keeps its first position and takes the last value (a Python dict).
fn parse_attrs(tag_line: &str) -> Vec<(String, String)> {
    let attr_str = tag_line.split_once(':').map_or("", |(_, rest)| rest);
    let mut out: Vec<(String, String)> = Vec::new();
    for captures in ATTR_RE.captures_iter(attr_str) {
        let key = &captures[1];
        let raw = &captures[2];
        let value = if raw.starts_with('"') && raw.ends_with('"') && !raw.is_empty() {
            // A lone `"` is both; Python's `raw[1:-1]` of it is "".
            if raw.len() == 1 {
                ""
            } else {
                &raw[1..raw.len() - 1]
            }
        } else {
            raw
        };
        match out.iter_mut().find(|(name, _)| name == key) {
            Some(entry) => entry.1 = value.to_string(),
            None => out.push((key.to_string(), value.to_string())),
        }
    }
    out
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// `_read_lines`: the text split on `\n`, without the empty tail a final newline leaves.
fn read_lines(path: &Path) -> Result<(Vec<String>, bool), HlsError> {
    let text = py::read_utf8(path)?;
    let had_trailing = text.ends_with('\n');
    let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
    if had_trailing {
        lines.pop();
    }
    Ok((lines, had_trailing))
}

fn join_lines(lines: &[String], had_trailing_newline: bool) -> String {
    let mut text = lines.join("\n");
    if had_trailing_newline {
        text.push('\n');
    }
    text
}

fn is_uri_line(line: &str) -> bool {
    !line.is_empty() && !line.starts_with('#')
}

/// `_seg_index`: the first run of decimal digits of the URL's stem, as Python's `int` prints
/// it (Unicode digits to ASCII, leading zeros dropped).
fn seg_index(url_or_name: &str) -> Result<String, HlsError> {
    let before_query = url_or_name.split('?').next().unwrap_or("");
    let stem = path::stem(before_query);
    let digits: String = stem
        .chars()
        .skip_while(|c| py::decimal_value(*c).is_none())
        .take_while(|c| py::decimal_value(*c).is_some())
        .collect();
    if digits.is_empty() {
        return rewrite_error(format!(
            "segment reference has no numeric index: {}",
            repr_str(url_or_name)
        ));
    }
    Ok(py::parse_int(&digits)?.to_string())
}

struct AudioMedia {
    line_index: usize,
    group_id: String,
    uri: String,
}

struct StreamInf {
    tag_line_index: usize,
    attrs: Vec<(String, String)>,
    uri: String,
}

struct ParsedMaster {
    lines: Vec<String>,
    had_trailing_newline: bool,
    audio_media: Vec<AudioMedia>,
    variants: Vec<StreamInf>,
}

/// `_reject_unmodeled`.
fn reject_unmodeled(lines: &[String]) -> Result<(), HlsError> {
    for line in lines {
        for prefix in UNMODELED_TAG_PREFIXES {
            if line.starts_with(prefix) {
                return rewrite_error(format!(
                    "unmodeled HLS construct {} -- refusing to pass through unverified \
                     (P2-HLS-UNMODELED-01)",
                    repr_str(prefix)
                ));
            }
        }
        if line.starts_with("#EXT-X-MEDIA:") {
            let attrs = parse_attrs(line);
            let media_type = attr(&attrs, "TYPE").unwrap_or("");
            if UNMODELED_MEDIA_TYPES.contains(&media_type) {
                return rewrite_error(format!(
                    "unmodeled HLS media group TYPE={media_type} -- refusing to pass through \
                     unverified (P2-HLS-UNMODELED-01)"
                ));
            }
        }
    }
    Ok(())
}

fn parse_master(path: &Path) -> Result<ParsedMaster, HlsError> {
    let (lines, had_trailing_newline) = read_lines(path)?;
    reject_unmodeled(&lines)?;
    let mut audio_media = Vec::new();
    let mut variants = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.starts_with("#EXT-X-MEDIA:") {
            let attrs = parse_attrs(line);
            if attr(&attrs, "TYPE") == Some("AUDIO") {
                audio_media.push(AudioMedia {
                    line_index: index,
                    group_id: attr(&attrs, "GROUP-ID").unwrap_or("").to_string(),
                    uri: attr(&attrs, "URI").unwrap_or("").to_string(),
                });
            }
        } else if line.starts_with("#EXT-X-STREAM-INF:") {
            let mut uri_index = None;
            for (next, candidate) in lines.iter().enumerate().skip(index + 1) {
                if is_uri_line(candidate) {
                    uri_index = Some(next);
                    break;
                }
                if candidate.starts_with("#EXT-X-STREAM-INF:") {
                    break;
                }
            }
            let Some(uri_index) = uri_index else {
                return rewrite_error(format!(
                    "#EXT-X-STREAM-INF at line {index} has no following URI line"
                ));
            };
            variants.push(StreamInf {
                tag_line_index: index,
                attrs: parse_attrs(line),
                uri: lines[uri_index].clone(),
            });
        }
    }
    Ok(ParsedMaster {
        lines,
        had_trailing_newline,
        audio_media,
        variants,
    })
}

/// `_playlist_container`: `m4s` when the playlist has an `#EXT-X-MAP`, else `ts`.
fn playlist_container(playlist_text: &str) -> &'static str {
    if py::splitlines(playlist_text)
        .iter()
        .any(|line| line.starts_with("#EXT-X-MAP:"))
    {
        "m4s"
    } else {
        "ts"
    }
}

fn segment_urls(playlist_text: &str) -> Vec<&str> {
    py::splitlines(playlist_text)
        .into_iter()
        .filter(|line| is_uri_line(line))
        .collect()
}

/// `_on_disk_total_size`: the sizes of the playlist's segments (init excluded).
fn on_disk_total_size(playlist_text: &str, download_root: &Path) -> Result<u64, HlsError> {
    let mut total = 0;
    for url in segment_urls(playlist_text) {
        total += py::file_size(&resolve_local_path(download_root, "", url)?)?;
    }
    Ok(total)
}

fn dash_total_size(rendition: &Rendition) -> Result<u64, HlsError> {
    let mut total = 0;
    for segment in &rendition.segment_paths {
        total += py::file_size(segment)?;
    }
    Ok(total)
}

/// `float(int)` of a byte total (Python rounds an int above 2**53 to the nearest float).
fn float_of_size(size: u64) -> f64 {
    BigInt::from(size)
        .to_string()
        .parse()
        .expect("a decimal integer parses")
}

// ------------------------------------------------------------------------------------------
// Multi-signal mapping (FAIL-loud)
// ------------------------------------------------------------------------------------------

/// `_codec_family`: the first codec's leading ASCII alphanumerics, four at most, lower-cased.
fn codec_family(codec: &str) -> String {
    let token = py::strip(codec.split(',').next().unwrap_or(""));
    let run: String = token
        .chars()
        .take_while(char::is_ascii_alphanumeric)
        .collect();
    if run.is_empty() {
        py::lower(token)
    } else {
        py::lower(&run.chars().take(4).collect::<String>())
    }
}

/// `_strict_order`: indices sorted by value; equal neighbours are an ambiguous rank.
fn strict_order(values: &[f64], what: &str, family: &str) -> Result<Vec<usize>, HlsError> {
    let order = py::sorted_indices(values);
    for pair in order.windows(2) {
        if py::float_eq(values[pair[0]], values[pair[1]]) {
            return rewrite_error(format!(
                "{what} cannot disambiguate the {} renditions: a tie at value {} \
                 (ambiguous multi-signal mapping; no tie-breaking)",
                repr_str(family),
                float_str(values[pair[0]])
            ));
        }
    }
    Ok(order)
}

fn required_bandwidth(variant: &StreamInf) -> Result<f64, HlsError> {
    let raw = attr(&variant.attrs, "BANDWIDTH");
    match raw.map(py::parse_float) {
        Some(Ok(value)) => Ok(value),
        _ => rewrite_error(format!(
            "#EXT-X-STREAM-INF is missing a numeric BANDWIDTH (got {}) -- cannot rank \
             (P2-HLS-DYN-01)",
            py::repr_opt(raw)
        )),
    }
}

fn video_content_gate(container: &str) -> &'static str {
    if container == "ts" {
        WAIVED_TS_H264
    } else {
        "framemd5-eq"
    }
}

/// `map_hls_variants_to_dash`: one mapping per surviving playlist (video variants, then the
/// fMP4 audio groups), FAIL-loud on any disagreement, tie or count mismatch. Read-only.
pub fn map_hls_variants_to_dash(
    master_path: &Path,
    dash_renditions: &[Rendition],
) -> Result<Vec<HlsVariantMapping>, HlsError> {
    let download_root = find_download_root(master_path)?;
    let master = parse_master(master_path)?;
    let mut mappings = map_video(&master, dash_renditions, &download_root)?;
    mappings.extend(map_audio(&master, dash_renditions, &download_root)?);
    Ok(mappings)
}

struct VideoSide<'a> {
    variants: Vec<&'a StreamInf>,
    playlists: Vec<PathBuf>,
    texts: Vec<String>,
    bandwidths: Vec<f64>,
    sizes: Vec<f64>,
}

fn gather_video_side<'a>(
    variants: Vec<&'a StreamInf>,
    download_root: &Path,
) -> Result<VideoSide<'a>, HlsError> {
    let mut side = VideoSide {
        variants: Vec::new(),
        playlists: Vec::new(),
        texts: Vec::new(),
        bandwidths: Vec::new(),
        sizes: Vec::new(),
    };
    for variant in variants {
        let playlist = resolve_local_path(download_root, "", &variant.uri)?;
        let text = py::read_text(&playlist)?;
        side.bandwidths.push(required_bandwidth(variant)?);
        side.sizes
            .push(float_of_size(on_disk_total_size(&text, download_root)?));
        side.playlists.push(playlist);
        side.texts.push(text);
        side.variants.push(variant);
    }
    Ok(side)
}

fn map_video(
    master: &ParsedMaster,
    dash_renditions: &[Rendition],
    download_root: &Path,
) -> Result<Vec<HlsVariantMapping>, HlsError> {
    let mut hls_by_family: Vec<(String, Vec<&StreamInf>)> = Vec::new();
    for variant in &master.variants {
        let family = codec_family(attr(&variant.attrs, "CODECS").unwrap_or(""));
        match hls_by_family.iter_mut().find(|(known, _)| *known == family) {
            Some((_, members)) => members.push(variant),
            None => hls_by_family.push((family, vec![variant])),
        }
    }
    let mut out = Vec::new();
    for (family, variants) in hls_by_family {
        let dash_family: Vec<&Rendition> = dash_renditions
            .iter()
            .filter(|r| r.content_type == "video" && codec_family(&r.codec) == family)
            .collect();
        if variants.len() != dash_family.len() {
            return rewrite_error(format!(
                "video family {}: {} HLS variants vs {} DASH renditions -- cannot map \
                 (P2-HLS-DYN-01, count mismatch)",
                repr_str(&family),
                variants.len(),
                dash_family.len()
            ));
        }
        let side = gather_video_side(variants, download_root)?;
        let bw_order = strict_order(&side.bandwidths, "HLS BANDWIDTH rank", &family)?;
        let size_order = strict_order(&side.sizes, "HLS on-disk size rank", &family)?;
        if bw_order != size_order {
            return rewrite_error(format!(
                "video family {}: HLS BANDWIDTH rank {} disagrees with on-disk size rank {} \
                 -- ambiguous mapping; no tie-breaking",
                repr_str(&family),
                py::repr_int_list(&bw_order),
                py::repr_int_list(&size_order)
            ));
        }
        let mut dash_sizes = Vec::new();
        for rendition in &dash_family {
            dash_sizes.push(float_of_size(dash_total_size(rendition)?));
        }
        let dash_order = strict_order(&dash_sizes, "DASH on-disk size rank", &family)?;
        for (rank, (h_idx, d_idx)) in size_order.iter().zip(&dash_order).enumerate() {
            let rendition = dash_family[*d_idx];
            out.push(join_video(
                &side,
                *h_idx,
                rendition,
                dash_sizes[*d_idx],
                rank,
                &family,
            )?);
        }
    }
    Ok(out)
}

fn join_video(
    side: &VideoSide<'_>,
    h_idx: usize,
    rendition: &Rendition,
    dash_size: f64,
    rank: usize,
    family: &str,
) -> Result<HlsVariantMapping, HlsError> {
    let variant = side.variants[h_idx];
    let container = playlist_container(&side.texts[h_idx]);
    assert_segment_count(&side.texts[h_idx], rendition, &side.playlists[h_idx])?;
    let metadata_signals = cross_check_video_metadata(&variant.attrs, rendition)?;
    let get = |key: &str| attr(&variant.attrs, key).unwrap_or("").to_string();
    let mut signals = vec![
        ("content_type".to_string(), "video".to_string()),
        ("codec_family".to_string(), family.to_string()),
        ("hls_codecs".to_string(), get("CODECS")),
        ("dash_codec".to_string(), rendition.codec.clone()),
        ("resolution".to_string(), get("RESOLUTION")),
        ("frame_rate".to_string(), get("FRAME-RATE")),
        (
            "hls_bandwidth".to_string(),
            py::int_of_float(side.bandwidths[h_idx])?.to_string(),
        ),
        ("hls_bandwidth_rank".to_string(), rank.to_string()),
        (
            "hls_size_bytes".to_string(),
            py::int_of_float(side.sizes[h_idx])?.to_string(),
        ),
        ("hls_size_rank".to_string(), rank.to_string()),
        (
            "dash_size_bytes".to_string(),
            py::int_of_float(dash_size)?.to_string(),
        ),
        ("dash_size_rank".to_string(), rank.to_string()),
        ("dash_rep_id".to_string(), rendition.rep_id.clone()),
        ("container".to_string(), container.to_string()),
    ];
    signals.extend(metadata_signals);
    Ok(HlsVariantMapping {
        hls_playlist: side.playlists[h_idx].clone(),
        dash_rendition: rendition.clone(),
        signals,
        content_gate: video_content_gate(container).to_string(),
    })
}

/// `_cross_check_video_metadata` : RESOLUTION ≡ width×height and FRAME-RATE within
/// half a 0.001 fps unit of the DASH rational corroborate the rank join, never override it.
fn cross_check_video_metadata(
    attrs: &[(String, String)],
    rendition: &Rendition,
) -> Result<Vec<(String, String)>, HlsError> {
    let mut signals = Vec::new();
    if let (Some(resolution), Some(width), Some(height)) = (
        attr(attrs, "RESOLUTION").filter(|value| !value.is_empty()),
        rendition.width,
        rendition.height,
    ) {
        let expected = format!("{width}x{height}");
        if resolution != expected {
            return rewrite_error("video resolution disagrees across DASH and HLS");
        }
        signals.push(("dash_resolution".to_string(), expected));
        signals.push(("resolution_check".to_string(), "matched".to_string()));
    }
    let hls_rate = attr(attrs, "FRAME-RATE").filter(|value| !value.is_empty());
    let dash_rate = rendition
        .frame_rate
        .as_deref()
        .filter(|value| !value.is_empty());
    if let (Some(hls_rate), Some(dash_rate)) = (hls_rate, dash_rate) {
        let agree = match (Fraction::parse(dash_rate), Fraction::parse(hls_rate)) {
            (Ok(dash), Ok(hls)) => {
                let bound = Fraction::new(1, 2000).expect("non-zero");
                dash.sign_cmp().is_gt() && hls.sign_cmp().is_gt() && (&dash - &hls).abs() <= bound
            }
            _ => false,
        };
        if !agree {
            return rewrite_error("video frame-rate disagrees across DASH and HLS");
        }
        signals.push(("dash_frame_rate".to_string(), dash_rate.to_string()));
        signals.push((
            "frame_rate_check".to_string(),
            "matched-within-0.0005-fps".to_string(),
        ));
    }
    Ok(signals)
}

/// `_language`: `und` for a missing or blank language, otherwise stripped and lower-cased.
fn language(value: Option<&str>) -> String {
    let lowered = py::lower(py::strip(value.filter(|v| !v.is_empty()).unwrap_or("und")));
    if lowered.is_empty() {
        "und".to_string()
    } else {
        lowered
    }
}

struct AudioSide<'a> {
    groups: Vec<&'a AudioMedia>,
    playlists: Vec<PathBuf>,
    texts: Vec<String>,
    sizes: Vec<f64>,
    languages: Vec<String>,
}

fn map_audio(
    master: &ParsedMaster,
    dash_renditions: &[Rendition],
    download_root: &Path,
) -> Result<Vec<HlsVariantMapping>, HlsError> {
    let groups: Vec<&AudioMedia> = master
        .audio_media
        .iter()
        .filter(|media| group_family(&media.group_id) == "fmp4_AUDIO")
        .collect();
    let dash_audio: Vec<&Rendition> = dash_renditions
        .iter()
        .filter(|r| r.content_type == "audio")
        .collect();
    if groups.is_empty() {
        return Ok(Vec::new());
    }
    if groups.len() != dash_audio.len() {
        return rewrite_error(format!(
            "audio: {} fmp4_AUDIO groups vs {} DASH audio renditions -- cannot map \
             (P2-HLS-DYN-01, count mismatch)",
            groups.len(),
            dash_audio.len()
        ));
    }
    let mut side = AudioSide {
        groups: Vec::new(),
        playlists: Vec::new(),
        texts: Vec::new(),
        sizes: Vec::new(),
        languages: Vec::new(),
    };
    for group in groups {
        let playlist = resolve_local_path(download_root, "", &group.uri)?;
        let text = py::read_text(&playlist)?;
        side.sizes
            .push(float_of_size(on_disk_total_size(&text, download_root)?));
        side.playlists.push(playlist);
        side.texts.push(text);
        side.groups.push(group);
    }
    let mut dash_sizes = Vec::new();
    for rendition in &dash_audio {
        dash_sizes.push(float_of_size(dash_total_size(rendition)?));
    }
    for group in &side.groups {
        let attrs = parse_attrs(&master.lines[group.line_index]);
        side.languages.push(language(attr(&attrs, "LANGUAGE")));
    }
    let dash_languages: Vec<String> = dash_audio
        .iter()
        .map(|r| language(r.lang.as_deref()))
        .collect();
    let mut all_languages: Vec<&String> = side.languages.iter().chain(&dash_languages).collect();
    all_languages.sort();
    all_languages.dedup();
    let mut out = Vec::new();
    for language in all_languages {
        let h_indices: Vec<usize> = (0..side.languages.len())
            .filter(|i| &side.languages[*i] == language)
            .collect();
        let d_indices: Vec<usize> = (0..dash_languages.len())
            .filter(|i| &dash_languages[*i] == language)
            .collect();
        if h_indices.len() != d_indices.len() {
            return rewrite_error("audio language counts disagree across DASH and HLS");
        }
        let h_values: Vec<f64> = h_indices.iter().map(|i| side.sizes[*i]).collect();
        let d_values: Vec<f64> = d_indices.iter().map(|i| dash_sizes[*i]).collect();
        let h_order = strict_order(&h_values, "HLS fmp4_AUDIO on-disk size rank", "audio")?;
        let d_order = strict_order(&d_values, "DASH audio on-disk size rank", "audio")?;
        for (rank, (h_local, d_local)) in h_order.iter().zip(&d_order).enumerate() {
            let (h_idx, d_idx) = (h_indices[*h_local], d_indices[*d_local]);
            let (group, rendition) = (side.groups[h_idx], dash_audio[d_idx]);
            assert_segment_count(&side.texts[h_idx], rendition, &side.playlists[h_idx])?;
            let signals = vec![
                ("content_type".to_string(), "audio".to_string()),
                ("language".to_string(), language.clone()),
                ("group_id".to_string(), group.group_id.clone()),
                ("group_index".to_string(), group_index(&group.group_id)),
                (
                    "hls_size_bytes".to_string(),
                    py::int_of_float(side.sizes[h_idx])?.to_string(),
                ),
                ("hls_size_rank".to_string(), rank.to_string()),
                (
                    "dash_size_bytes".to_string(),
                    py::int_of_float(dash_sizes[d_idx])?.to_string(),
                ),
                ("dash_size_rank".to_string(), rank.to_string()),
                ("dash_rep_id".to_string(), rendition.rep_id.clone()),
                ("container".to_string(), "m4s".to_string()),
            ];
            out.push(HlsVariantMapping {
                hls_playlist: side.playlists[h_idx].clone(),
                dash_rendition: rendition.clone(),
                signals,
                content_gate: "pcm-eq".to_string(),
            });
        }
    }
    Ok(out)
}

/// `_assert_segment_count`.
fn assert_segment_count(
    playlist_text: &str,
    rendition: &Rendition,
    playlist_path: &Path,
) -> Result<(), HlsError> {
    let hls_count = segment_urls(playlist_text).len();
    let dash_count = rendition.segment_paths.len();
    if hls_count != dash_count {
        return rewrite_error(format!(
            "segment-count mismatch for {}: HLS={hls_count} vs DASH CBCS={dash_count} \
             (P2-HLS-SEGCOUNT-01)",
            path::name(&path::fspath(playlist_path)?)
        ));
    }
    Ok(())
}

/// `_AUDIO_GROUP_INDEX_RE.match(group_id)` (`^(?P<family>.+)-(?P<index>\d+)$`): the family and
/// index of `fmp4_AUDIO-0`. `.+` is greedy and stops at a newline; `$` also matches before a
/// final newline.
fn group_parts(group_id: &str) -> Option<(&str, &str)> {
    let line_end = group_id.find('\n').unwrap_or(group_id.len());
    let candidates = group_id[..line_end].match_indices('-').rev();
    for (dash, _) in candidates {
        if dash == 0 {
            continue;
        }
        let rest = &group_id[dash + 1..];
        let digits_end = rest
            .char_indices()
            .find(|(_, c)| py::decimal_value(*c).is_none())
            .map_or(rest.len(), |(index, _)| index);
        let tail = &rest[digits_end..];
        if digits_end > 0 && (tail.is_empty() || tail == "\n") {
            return Some((&group_id[..dash], &rest[..digits_end]));
        }
    }
    None
}

fn group_family(group_id: &str) -> &str {
    group_parts(group_id).map_or(group_id, |(family, _)| family)
}

fn group_index(group_id: &str) -> String {
    group_parts(group_id).map_or_else(String::new, |(_, index)| index.to_string())
}

/// `find_audio_consolidation_pairs`: the `(ts_playlist, fmp4_playlist)` pairs
/// `rewrite_hls_master` consolidates (matched by group index; an unpaired `ts_AUDIO-N` is
/// skipped here — `rewrite_hls_master` is what fails on it). Read-only.
pub fn find_audio_consolidation_pairs(
    master_path: &Path,
) -> Result<Vec<(PathBuf, PathBuf)>, HlsError> {
    let download_root = find_download_root(master_path)?;
    let master = parse_master(master_path)?;
    let mut fmp4_by_index: Vec<(String, &AudioMedia)> = Vec::new();
    for media in &master.audio_media {
        if group_family(&media.group_id) != "fmp4_AUDIO" {
            continue;
        }
        let index = group_index(&media.group_id);
        match fmp4_by_index.iter_mut().find(|(known, _)| *known == index) {
            Some(entry) => entry.1 = media,
            None => fmp4_by_index.push((index, media)),
        }
    }
    let mut pairs = Vec::new();
    for media in &master.audio_media {
        if group_family(&media.group_id) != "ts_AUDIO" {
            continue;
        }
        let index = group_index(&media.group_id);
        let Some((_, fmp4)) = fmp4_by_index.iter().find(|(known, _)| *known == index) else {
            continue;
        };
        pairs.push((
            resolve_local_path(&download_root, "", &media.uri)?,
            resolve_local_path(&download_root, "", &fmp4.uri)?,
        ));
    }
    Ok(pairs)
}

// ------------------------------------------------------------------------------------------
// Master rewrite (audio consolidation)
// ------------------------------------------------------------------------------------------

/// `rewrite_hls_master`: consolidate the audio groups index-wise and drop the `ts_AUDIO-*`
/// groups, line by line. `url_base` is accepted for symmetry and unused (the master's URLs are
/// externally addressed). Read-only input; returns `output_master`.
pub fn rewrite_hls_master(
    source_master: &Path,
    output_master: &Path,
    url_base: Option<&str>,
) -> Result<PathBuf, HlsError> {
    let _ = url_base;
    let master = parse_master(source_master)?;
    let mut lines = master.lines.clone();
    let fmp4_indices: Vec<String> = master
        .audio_media
        .iter()
        .filter(|media| group_family(&media.group_id) == "fmp4_AUDIO")
        .map(|media| group_index(&media.group_id))
        .collect();
    let ts_groups: Vec<&AudioMedia> = master
        .audio_media
        .iter()
        .filter(|media| group_family(&media.group_id) == "ts_AUDIO")
        .collect();
    for media in &ts_groups {
        let index = group_index(&media.group_id);
        if !fmp4_indices.contains(&index) {
            return rewrite_error(format!(
                "ts_AUDIO group {} has no matching fmp4_AUDIO-{index} to consolidate onto \
                 (P2-HLS-AUDIO-01)",
                repr_str(&media.group_id)
            ));
        }
    }
    for variant in &master.variants {
        let Some(audio_group) = attr(&variant.attrs, "AUDIO").filter(|group| !group.is_empty())
        else {
            continue;
        };
        if group_family(audio_group) != "ts_AUDIO" {
            continue;
        }
        let new_group = format!("fmp4_AUDIO-{}", group_index(audio_group));
        let old_attr = format!("AUDIO=\"{audio_group}\"");
        let new_attr = format!("AUDIO=\"{new_group}\"");
        let line = &lines[variant.tag_line_index];
        if !line.contains(&old_attr) {
            return rewrite_error(format!(
                "could not locate {} on STREAM-INF line {}",
                repr_str(&old_attr),
                variant.tag_line_index
            ));
        }
        lines[variant.tag_line_index] = line.replacen(&old_attr, &new_attr, 1);
    }
    let mut remove: Vec<usize> = ts_groups.iter().map(|media| media.line_index).collect();
    remove.sort_unstable_by(|a, b| b.cmp(a));
    for index in remove {
        lines.remove(index);
    }
    assert_no_dangling_audio(&lines)?;
    py::mkdir_parents(&parent_dir(output_master)?)?;
    py::write_bytes(
        output_master,
        join_lines(&lines, master.had_trailing_newline).as_bytes(),
    )?;
    Ok(output_master.to_path_buf())
}

fn parent_dir(file: &Path) -> Result<PathBuf, HlsError> {
    Ok(PathBuf::from(path::parent(&path::fspath(file)?)))
}

fn assert_no_dangling_audio(lines: &[String]) -> Result<(), HlsError> {
    let present: Vec<String> = lines
        .iter()
        .filter(|line| line.starts_with("#EXT-X-MEDIA:"))
        .map(|line| parse_attrs(line))
        .filter(|attrs| attr(attrs, "TYPE") == Some("AUDIO"))
        .map(|attrs| attr(&attrs, "GROUP-ID").unwrap_or("").to_string())
        .collect();
    for line in lines
        .iter()
        .filter(|line| line.starts_with("#EXT-X-STREAM-INF:"))
    {
        let attrs = parse_attrs(line);
        if let Some(referenced) = attr(&attrs, "AUDIO").filter(|group| !group.is_empty())
            && !present.iter().any(|group| group == referenced)
        {
            return rewrite_error(format!(
                "dangling AUDIO={} after consolidation -- no such group remains \
                 (P2-HLS-AUDIO-01)",
                repr_str(referenced)
            ));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------
// Media-playlist rewrite (EXT-X-MAP + segment repoint + IV verify)
// ------------------------------------------------------------------------------------------

/// `_target_base_url`: the CBCS directory URL (no trailing slash) of the rendition — the
/// DASH-side production URL rebuilt from the mirrored path (`None`), or `url_base` + `rel_dir`.
fn target_base_url(rendition: &Rendition, url_base: Option<&str>) -> Result<String, HlsError> {
    if let Some(url_base) = url_base {
        let rel_dir = path::pure(&path::fspath(&rendition.rel_dir)?);
        return Ok(format!("{}/{rel_dir}", url_base.trim_end_matches('/')));
    }
    let download_root = find_download_root(&rendition.init_path)?;
    let init_parent = path::parent(&path::fspath(&rendition.init_path)?);
    let resolved = path::fspath(&path::resolve(Path::new(&init_parent))?)?;
    let root = path::fspath(&path::resolve(&download_root)?)?;
    Ok(format!("https://{}", path::relative_to(&resolved, &root)?))
}

/// `_iv_from_key_line`: the `IV=` value lower-cased, `0x` dropped.
fn iv_from_key_line(line: &str) -> Option<String> {
    let attrs = parse_attrs(line);
    let iv = attr(&attrs, "IV")?;
    let lowered = py::lower(iv);
    Some(match lowered.strip_prefix("0x") {
        Some(rest) => rest.to_string(),
        None => lowered,
    })
}

/// `rewrite_hls_media_playlist`: add or repoint `#EXT-X-MAP`, repoint every segment URL onto
/// the rendition's CBCS directory, verify the `#EXT-X-KEY` IV against `expected_iv_hex` (never
/// rewritten, never printed). Read-only input; returns `output_playlist`.
pub fn rewrite_hls_media_playlist(
    source_playlist: &Path,
    output_playlist: &Path,
    dash_rendition: &Rendition,
    expected_iv_hex: &str,
    url_base: Option<&str>,
) -> Result<PathBuf, HlsError> {
    let (mut lines, had_trailing) = read_lines(source_playlist)?;
    let base = target_base_url(dash_rendition, url_base)?;
    let lowered = py::lower(expected_iv_hex);
    let expected = lowered.strip_prefix("0x").unwrap_or(&lowered).to_string();
    let mut key_verified = false;
    let mut map_present = false;
    let mut key_line_index = None;
    for (index, line) in lines.iter_mut().enumerate() {
        if line.starts_with("#EXT-X-KEY:") {
            key_line_index = Some(index);
            let Some(actual) = iv_from_key_line(line) else {
                return rewrite_error("EXT-X-KEY has no IV= to verify (P2-HLS-IV-01)");
            };
            if actual != expected {
                return rewrite_error(
                    "EXT-X-KEY IV does not match the configured/DASH-tenc IV -- refusing to \
                     repoint a playlist whose key material disagrees (P2-HLS-IV-01)",
                );
            }
            key_verified = true;
        } else if line.starts_with("#EXT-X-MAP:") {
            map_present = true;
            let attrs = parse_attrs(line);
            let Some(old_uri) = attr(&attrs, "URI").filter(|uri| !uri.is_empty()) else {
                return rewrite_error("EXT-X-MAP has no URI= to repoint");
            };
            *line = line.replacen(
                &format!("URI=\"{old_uri}\""),
                &format!("URI=\"{base}/init.mp4\""),
                1,
            );
        } else if is_uri_line(line) {
            *line = format!("{base}/seg_{}.m4s", seg_index(line)?);
        }
    }
    let Some(key_line_index) = key_line_index.filter(|_| key_verified) else {
        return rewrite_error("media playlist has no #EXT-X-KEY to verify (P2-HLS-IV-01)");
    };
    if !map_present {
        lines.insert(
            key_line_index,
            format!("#EXT-X-MAP:URI=\"{base}/init.mp4\""),
        );
    }
    py::mkdir_parents(&parent_dir(output_playlist)?)?;
    py::write_bytes(output_playlist, join_lines(&lines, had_trailing).as_bytes())?;
    Ok(output_playlist.to_path_buf())
}

// ------------------------------------------------------------------------------------------
// Orchestration
// ------------------------------------------------------------------------------------------

/// `_extinf_values`: every `#EXTINF:` duration, `float` of the text before a trailing `,`.
fn extinf_values(playlist_text: &str) -> Result<Vec<f64>, HlsError> {
    py::splitlines(playlist_text)
        .into_iter()
        .filter_map(|line| line.strip_prefix("#EXTINF:"))
        .map(|value| Ok(py::parse_float(value.trim_end_matches(','))?))
        .collect()
}

/// The output path mirroring `source` (resolved) under `output_root`.
fn mirrored(output_root: &Path, source: &Path, download_root: &Path) -> Result<PathBuf, HlsError> {
    let source = path::fspath(&path::resolve(source)?)?;
    let root = path::fspath(&path::resolve(download_root)?)?;
    Ok(output_root.join(path::relative_to(&source, &root)?))
}

/// `rewrite_hls`: map, rewrite the master, repoint every surviving playlist under
/// `output_root` (mirroring the download-relative layout), and report the structural checks
/// (the decrypt / measurement gates as `SKIP` rows). Read-only input.
pub fn rewrite_hls(
    master_path: &Path,
    dash_renditions: &[Rendition],
    output_root: &Path,
    expected_iv_hex: &str,
    url_base: Option<&str>,
) -> Result<HlsRewriteResult, HlsError> {
    let download_root = find_download_root(master_path)?;
    let mappings = map_hls_variants_to_dash(master_path, dash_renditions)?;
    let source_master = parse_master(master_path)?;
    let ts_audio_groups_removed = source_master
        .audio_media
        .iter()
        .filter(|media| group_family(&media.group_id) == "ts_AUDIO")
        .count();
    let audio_groups_consolidated = source_master
        .variants
        .iter()
        .filter(|variant| group_family(attr(&variant.attrs, "AUDIO").unwrap_or("")) == "ts_AUDIO")
        .count();
    let output_master = mirrored(output_root, master_path, &download_root)?;
    rewrite_hls_master(master_path, &output_master, url_base)?;
    let mut output_playlists = Vec::new();
    let mut ext_x_map_added = 0;
    let mut segment_refs_repointed = 0;
    for mapping in &mappings {
        let source_text = py::read_text(&mapping.hls_playlist)?;
        let out_path = mirrored(output_root, &mapping.hls_playlist, &download_root)?;
        rewrite_hls_media_playlist(
            &mapping.hls_playlist,
            &out_path,
            &mapping.dash_rendition,
            expected_iv_hex,
            url_base,
        )?;
        output_playlists.push(out_path);
        if playlist_container(&source_text) == "ts" {
            ext_x_map_added += 1;
        }
        segment_refs_repointed += segment_urls(&source_text).len();
    }
    let versions = check_versions(master_path, &output_master, &output_playlists)?;
    let untouched = check_master_untouched(master_path, &output_master)?;
    let checks = vec![
        (CheckId::HlsIvAgreement, "PASS".to_string()),
        (CheckId::HlsSegmentCount, "PASS".to_string()),
        (CheckId::HlsVersionUnchanged, versions),
        (CheckId::AudioConsolidationStructure, "PASS".to_string()),
        (
            CheckId::AudioPcmIdentity,
            "SKIP: run audio_pcm_consolidation_gate with key material".to_string(),
        ),
        (
            CheckId::HlsContentGate,
            "SKIP: run content_identity_gate with key material + DASH clear output".to_string(),
        ),
        (
            CheckId::HlsExtinf,
            "SKIP: run check_extinf_gate (mp4dump/ffprobe over the CBCS output)".to_string(),
        ),
        (CheckId::HlsMasterUntouched, untouched),
    ];
    Ok(HlsRewriteResult {
        output_master_path: output_master,
        output_media_playlist_paths: output_playlists,
        audio_groups_consolidated,
        ts_audio_groups_removed,
        ext_x_map_added,
        segment_refs_repointed,
        iv_verified: true,
        variant_mapping: mappings,
        checks,
    })
}

/// `_check_versions` : the master's `#EXT-X-VERSION` line unchanged, every
/// media playlist at `#EXT-X-VERSION:7`.
fn check_versions(
    master_path: &Path,
    output_master: &Path,
    output_playlists: &[PathBuf],
) -> Result<String, HlsError> {
    let source = py::read_text(master_path)?;
    let output = py::read_text(output_master)?;
    if version_line(&source) != version_line(&output) {
        return Ok("FAIL: master #EXT-X-VERSION changed".to_string());
    }
    for playlist in output_playlists {
        let text = py::read_text(playlist)?;
        if version_line(&text) != Some("#EXT-X-VERSION:7") {
            return Ok(format!(
                "FAIL: media playlist {} not #EXT-X-VERSION:7",
                path::name(&path::fspath(playlist)?)
            ));
        }
    }
    Ok("PASS".to_string())
}

fn version_line(text: &str) -> Option<&str> {
    py::splitlines(text)
        .into_iter()
        .find(|line| line.starts_with("#EXT-X-VERSION:"))
}

/// `_check_master_untouched` : every changed line is a removed
/// `ts_AUDIO` `#EXT-X-MEDIA` or a flipped `#EXT-X-STREAM-INF`.
fn check_master_untouched(master_path: &Path, output_master: &Path) -> Result<String, HlsError> {
    let source = py::read_text(master_path)?;
    let output = py::read_text(output_master)?;
    let source_lines: Vec<&str> = source.split('\n').collect();
    let output_lines: Vec<&str> = output.split('\n').collect();
    for line in source_lines
        .iter()
        .filter(|line| !output_lines.contains(line))
    {
        let is_ts_media = line.starts_with("#EXT-X-MEDIA:") && line.contains("ts_AUDIO");
        if !(is_ts_media || line.starts_with("#EXT-X-STREAM-INF:")) {
            return Ok(
                "FAIL: master changed a line other than an AUDIO= flip / ts_AUDIO removal"
                    .to_string(),
            );
        }
    }
    let added_other = output_lines
        .iter()
        .filter(|line| !source_lines.contains(line))
        .any(|line| !line.starts_with("#EXT-X-STREAM-INF:"));
    if added_other {
        return Ok("FAIL: master added a non-STREAM-INF line".to_string());
    }
    Ok("PASS".to_string())
}

// ------------------------------------------------------------------------------------------
// Gates
// ------------------------------------------------------------------------------------------

/// `check_extinf_gate` : each `#EXTINF` within `tolerance_s` of the measured
/// duration of the mapped rendition's segment. A mismatch is a `FAIL` with the measurements.
pub fn check_extinf_gate(
    mapping: &HlsVariantMapping,
    tolerance_s: f64,
    media: &dyn GateMedia,
) -> Result<String, HlsError> {
    let playlist_text = py::read_text(&mapping.hls_playlist)?;
    let extinf = extinf_values(&playlist_text)?;
    let rendition = &mapping.dash_rendition;
    let measured = media.measure_segment_durations_s(
        &rendition.init_path,
        &rendition.segment_paths,
        rendition.timescale,
    )?;
    if extinf.len() != measured.len() {
        return Ok(format!(
            "FAIL: EXTINF count {} != measured segment count {}",
            extinf.len(),
            measured.len()
        ));
    }
    let (mut worst_index, mut worst) = (-1_i64, 0.0_f64);
    for (index, (declared, actual)) in extinf.iter().zip(&measured).enumerate() {
        let delta = (declared - actual).abs();
        if delta > worst {
            worst_index = i64::try_from(index).expect("fewer than 2**63 segments");
            worst = delta;
        }
    }
    if worst > tolerance_s {
        return Ok(format!(
            "FAIL: segment {worst_index} EXTINF diverges from measured CBCS duration by {}s \
             (> {}s) -- inspect the timing measurements before rewriting",
            format_fixed(worst, 6),
            float_str(tolerance_s)
        ));
    }
    Ok("PASS".to_string())
}

/// `_decode_hls_variant`: localize the playlist (identity key file + local files) and decode
/// it to PCM or a framemd5 report under `work_dir`. The key touches only the key file.
fn decode_hls_variant(
    playlist_path: &Path,
    key: &KeyMaterial,
    work_dir: &Path,
    mode: &str,
    media: &dyn GateMedia,
) -> Result<PathBuf, HlsError> {
    py::mkdir_parents(work_dir)?;
    let download_root = find_download_root(playlist_path)?;
    let key_file = media.write_raw_key_file(key.key_hex(), &work_dir.join("content.key"))?;
    let resolve = |url: &str| resolve_local_path(&download_root, "", url);
    let localized_text =
        media.localize_hls_media_playlist(&py::read_text(playlist_path)?, &key_file, &resolve)?;
    let localized = work_dir.join("localized.m3u8");
    py::write_bytes(&localized, localized_text.as_bytes())?;
    let extension = if mode == "pcm" { "pcm" } else { "framemd5" };
    Ok(media.decode_hls_media_playlist(
        &localized,
        &work_dir.join(format!("decoded.{extension}")),
        mode,
    )?)
}

/// The `RuntimeError` of a gate step as Python's `except RuntimeError` sees it.
fn runtime_message(err: &HlsError) -> Option<String> {
    match err {
        HlsError::Py(PyErr::Runtime(message)) => Some(message.clone()),
        _ => None,
    }
}

/// `audio_pcm_consolidation_gate` (checklist 6-2): `ts_AUDIO-N` may be
/// consolidated onto `fmp4_AUDIO-N` only if both decode to identical PCM.
pub fn audio_pcm_consolidation_gate(
    ts_playlist_path: &Path,
    fmp4_playlist_path: &Path,
    key: &KeyMaterial,
    work_dir: &Path,
    media: &dyn GateMedia,
) -> Result<String, HlsError> {
    let decoded = decode_hls_variant(ts_playlist_path, key, &work_dir.join("ts"), "pcm", media)
        .and_then(|ts_pcm| {
            let fmp4_pcm = decode_hls_variant(
                fmp4_playlist_path,
                key,
                &work_dir.join("fmp4"),
                "pcm",
                media,
            )?;
            Ok((ts_pcm, fmp4_pcm))
        });
    let (ts_pcm, fmp4_pcm) = match decoded {
        Ok(pair) => pair,
        Err(err) => {
            return match runtime_message(&err) {
                Some(message) => Ok(format!("FAIL: audio decode error: {message}")),
                None => Err(err),
            };
        }
    };
    if py::read_bytes(&ts_pcm)? == py::read_bytes(&fmp4_pcm)? {
        return Ok("PASS".to_string());
    }
    Ok(
        "FAIL: ts_AUDIO and fmp4_AUDIO decode to different PCM -- consolidation is INVALID"
            .to_string(),
    )
}

/// `content_identity_gate` : an fMP4 variant's decrypted content equals the
/// mapped DASH clear output (framemd5 for video, PCM for audio); waived for TS H.264.
pub fn content_identity_gate(
    mapping: &HlsVariantMapping,
    dash_clear_path: &Path,
    key: &KeyMaterial,
    work_dir: &Path,
    media: &dyn GateMedia,
) -> Result<String, HlsError> {
    if mapping.content_gate == WAIVED_TS_H264 {
        return Ok("SKIP: TS H.264 decryption is not supported; \
                   segment count, duration, and key signaling checked separately"
            .to_string());
    }
    let outcome = if mapping.content_gate == "framemd5-eq" {
        content_framemd5(mapping, dash_clear_path, key, work_dir, media)
    } else {
        content_pcm(mapping, dash_clear_path, key, work_dir, media)
    };
    match outcome {
        Err(err) => match runtime_message(&err) {
            Some(message) => Ok(format!("FAIL: content-gate decode error: {message}")),
            None => Err(err),
        },
        ok => ok,
    }
}

fn content_framemd5(
    mapping: &HlsVariantMapping,
    dash_clear_path: &Path,
    key: &KeyMaterial,
    work_dir: &Path,
    media: &dyn GateMedia,
) -> Result<String, HlsError> {
    let report = decode_hls_variant(&mapping.hls_playlist, key, work_dir, "framemd5", media)?;
    let hls_md5 = py::read_text(&report)?;
    let dash_md5 = media.compute_framemd5(dash_clear_path)?;
    Ok(if hls_md5 == dash_md5 {
        "PASS".to_string()
    } else {
        "FAIL: HLS-side vs DASH-clear framemd5 mismatch".to_string()
    })
}

fn content_pcm(
    mapping: &HlsVariantMapping,
    dash_clear_path: &Path,
    key: &KeyMaterial,
    work_dir: &Path,
    media: &dyn GateMedia,
) -> Result<String, HlsError> {
    let hls_pcm = decode_hls_variant(&mapping.hls_playlist, key, work_dir, "pcm", media)?;
    let dash_pcm = media.decode_media_to_pcm(dash_clear_path, &work_dir.join("dash_clear.pcm"))?;
    Ok(if py::read_bytes(&hls_pcm)? == py::read_bytes(&dash_pcm)? {
        "PASS".to_string()
    } else {
        "FAIL: HLS-side vs DASH-clear PCM mismatch".to_string()
    })
}

// ------------------------------------------------------------------------------------------
// analyze_hls_manifest: the inspector's pure-parse summary (read-only parsing)
// ------------------------------------------------------------------------------------------

/// `HlsVariantSummary`: a `video-variant` / `audio-group` row of a master, or the one
/// `media-playlist` row of a media playlist. `key_iv` is the public IV as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlsVariantSummary {
    /// `video-variant` | `audio-group` | `media-playlist`.
    pub kind: String,
    /// The variant / group URI.
    pub uri: Option<String>,
    /// `CODECS`.
    pub codecs: Option<String>,
    /// `RESOLUTION`.
    pub resolution: Option<String>,
    /// `int(float(BANDWIDTH))`, `None` when not numeric.
    pub bandwidth: Option<BigInt>,
    /// `STREAM-INF AUDIO=`.
    pub audio_group: Option<String>,
    /// `EXT-X-MEDIA GROUP-ID`.
    pub group_id: Option<String>,
    /// `ts` | `m4s`.
    pub container: Option<String>,
    /// An `#EXT-X-MAP` is present.
    pub has_ext_x_map: bool,
    /// `EXT-X-KEY METHOD`.
    pub key_method: Option<String>,
    /// `EXT-X-KEY KEYFORMAT`.
    pub key_format: Option<String>,
    /// `EXT-X-KEY IV`, as written.
    pub key_iv: Option<String>,
    /// `EXT-X-VERSION`.
    pub version: Option<BigInt>,
}

/// `HlsManifestSummary`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HlsManifestSummary {
    /// The file has an `#EXT-X-STREAM-INF`.
    pub is_master: bool,
    /// `EXT-X-VERSION`.
    pub version: Option<BigInt>,
    /// The rows.
    pub variants: Vec<HlsVariantSummary>,
}

fn hls_version_int(text: &str) -> Option<BigInt> {
    let line = version_line(text)?;
    let value = line.split_once(':').map_or("", |(_, value)| value);
    py::parse_int(value).ok()
}

/// `_tolerant_bandwidth`: `int(float(raw))`, `None` for a missing or non-numeric value; an
/// infinite one is Python's `OverflowError`, which escapes.
fn tolerant_bandwidth(raw: Option<&str>) -> Result<Option<BigInt>, HlsError> {
    let Some(Ok(value)) = raw.map(py::parse_float) else {
        return Ok(None);
    };
    match py::int_of_float(value) {
        Ok(int) => Ok(Some(int)),
        Err(err) if err.is_value_error() => Ok(None),
        Err(err) => Err(HlsError::Py(err)),
    }
}

fn row(kind: &str, version: Option<&BigInt>) -> HlsVariantSummary {
    HlsVariantSummary {
        kind: kind.to_string(),
        uri: None,
        codecs: None,
        resolution: None,
        bandwidth: None,
        audio_group: None,
        group_id: None,
        container: None,
        has_ext_x_map: false,
        key_method: None,
        key_format: None,
        key_iv: None,
        version: version.cloned(),
    }
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

/// `analyze_hls_manifest`: the inspector's summary of one HLS file — a master (video-variant
/// and audio-group rows, the unmodeled-construct guard included) or a media playlist (one row).
/// Reads only `path`.
pub fn analyze_hls_manifest(path: &Path) -> Result<HlsManifestSummary, HlsError> {
    let (lines, had_trailing_newline) = read_lines(path)?;
    let text = join_lines(&lines, had_trailing_newline);
    let version = hls_version_int(&text);
    let is_master = lines
        .iter()
        .any(|line| line.starts_with("#EXT-X-STREAM-INF:"));
    if is_master {
        let master = parse_master(path)?;
        let mut variants = Vec::new();
        for variant in &master.variants {
            let mut summary = row("video-variant", version.as_ref());
            summary.uri = non_empty(&variant.uri);
            summary.codecs = attr(&variant.attrs, "CODECS").map(str::to_string);
            summary.resolution = attr(&variant.attrs, "RESOLUTION").map(str::to_string);
            summary.bandwidth = tolerant_bandwidth(attr(&variant.attrs, "BANDWIDTH"))?;
            summary.audio_group = attr(&variant.attrs, "AUDIO").map(str::to_string);
            variants.push(summary);
        }
        for media in &master.audio_media {
            let mut summary = row("audio-group", version.as_ref());
            summary.uri = non_empty(&media.uri);
            summary.group_id = non_empty(&media.group_id);
            variants.push(summary);
        }
        return Ok(HlsManifestSummary {
            is_master: true,
            version,
            variants,
        });
    }
    let mut key_attrs: Vec<(String, String)> = Vec::new();
    let mut has_ext_x_map = false;
    for line in &lines {
        if line.starts_with("#EXT-X-KEY:") {
            key_attrs = parse_attrs(line);
        } else if line.starts_with("#EXT-X-MAP:") {
            has_ext_x_map = true;
        }
    }
    let mut summary = row("media-playlist", version.as_ref());
    summary.container = Some(playlist_container(&text).to_string());
    summary.has_ext_x_map = has_ext_x_map;
    summary.key_method = attr(&key_attrs, "METHOD").map(str::to_string);
    summary.key_format = attr(&key_attrs, "KEYFORMAT").map(str::to_string);
    summary.key_iv = attr(&key_attrs, "IV").map(str::to_string);
    Ok(HlsManifestSummary {
        is_master: false,
        version,
        variants: vec![summary],
    })
}
