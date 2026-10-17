//! Patch DASH protection descriptors while preserving unrelated manifest text.
//!
//! For supported adaptations, change the encryption scheme from CENC to CBCS and replace
//! `PlayReady` PRO and PSSH representations together. Keep the key identifier, Widevine
//! PSSH, segment templates, and unsupported adaptations unchanged. XML parsing locates
//! edits, which are then applied to the original text.

use crate::checks::CheckId;
use crate::config::{IN_SCOPE_MIME_TYPES, in_scope_codec_prefixes};
use crate::manifest_support::fraction::Fraction;
use crate::manifest_support::path;
use crate::manifest_support::py::{self, PyErr, repr_opt};
use crate::manifest_support::xml::{self, Element};
use crate::playready::{ParseProError, ParsedPro, PlayReadyError, PlayReadyObjects, parse_pro};
use num_bigint::BigInt;
use std::path::{Path, PathBuf};

const CENC_NS: &str = "urn:mpeg:cenc:2013";
const MP4PROTECTION_SCHEME: &str = "urn:mpeg:dash:mp4protection:2011";
const PLAYREADY_SCHEME: &str = "urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95";
const WIDEVINE_SCHEME: &str = "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";
const ADAPTATION_SET_OPEN: &str = "<AdaptationSet";
const ADAPTATION_SET_CLOSE: &str = "</AdaptationSet>";

/// `DashRewriteError`: a malformed or unmodeled manifest (FAIL-loud; a Python `ValueError`).
/// The message names the structural problem only, never a PRO / pssh blob or key material.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DashRewriteError(pub String);

/// Every exception the functions can raise, by class; `Display` is `str(exc)`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DashError {
    /// `DashRewriteError`.
    #[error(transparent)]
    Rewrite(#[from] DashRewriteError),
    /// `playready.PlayReadyError` (from `parse_pro` in `parse_source_playready`).
    #[error(transparent)]
    PlayReady(#[from] PlayReadyError),
    /// A built-in exception: `OSError`, `UnicodeDecodeError`, the `ValueError` of `int` on an
    /// attribute, `OverflowError`.
    #[error(transparent)]
    Py(#[from] PyErr),
}

impl DashError {
    /// Whether Python's `except ValueError` catches it.
    pub fn is_value_error(&self) -> bool {
        match self {
            DashError::Rewrite(_) | DashError::PlayReady(_) => true,
            DashError::Py(err) => err.is_value_error(),
        }
    }
}

/// `parse_pro` raises `PlayReadyError` for every structural problem and lets the UTF-16-LE
/// codec's `UnicodeDecodeError` through (both `ValueError`s; only the first is
/// `PlayReadyError`).
impl From<ParseProError> for DashError {
    fn from(err: ParseProError) -> Self {
        match err {
            ParseProError::PlayReady(err) => DashError::PlayReady(err),
            ParseProError::UnicodeDecode(message) => DashError::Py(PyErr::UnicodeDecode(message)),
        }
    }
}

fn rewrite_error<T>(message: impl Into<String>) -> Result<T, DashError> {
    Err(DashError::Rewrite(DashRewriteError(message.into())))
}

/// `DashRewriteResult` (no key or IV field).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashRewriteResult {
    /// The written manifest.
    pub output_mpd_path: PathBuf,
    /// In-scope `AdaptationSets` whose scheme is `cbcs` in the output (stable across re-runs).
    pub adaptation_sets_patched: usize,
    /// The written `<cenc:pssh>` (`playready.pssh_b64`).
    pub playready_pssh_b64: String,
    /// The written `<mspr:pro>` (`playready.pro_b64`).
    pub playready_pro_b64: String,
    /// Every Widevine `<cenc:pssh>` byte-identical source → output.
    pub widevine_unchanged: bool,
    /// Check id → `PASS` | `FAIL: …` | `SKIP: …`, in Python's insertion order.
    pub checks: Vec<(CheckId, String)>,
}

// ------------------------------------------------------------------------------------------
// Local-name helpers (namespace-agnostic, as `_iter_local` is)
// ------------------------------------------------------------------------------------------

/// `_find_content_protection`: the first `ContentProtection` whose lower-cased
/// `@schemeIdUri` is `scheme_uri`.
fn find_content_protection<'a>(root: &'a Element, scheme_uri: &str) -> Option<&'a Element> {
    root.iter_local("ContentProtection")
        .into_iter()
        .find(|cp| py::lower(cp.get("schemeIdUri").unwrap_or("")) == scheme_uri)
}

/// `_child_text`: the `.text` of the first element named `local` under (or at) `cp`.
fn child_text<'a>(cp: Option<&'a Element>, local: &str) -> Option<&'a str> {
    cp?.iter_local(local).first()?.text.as_deref()
}

fn mp4protection_value(adaptation_set: &Element) -> Option<&str> {
    find_content_protection(adaptation_set, MP4PROTECTION_SCHEME)?.get("value")
}

fn default_kid(adaptation_set: &Element) -> Option<&str> {
    find_content_protection(adaptation_set, MP4PROTECTION_SCHEME)?
        .get(&format!("{{{CENC_NS}}}default_KID"))
}

fn playready_texts(adaptation_set: &Element) -> (Option<&str>, Option<&str>) {
    let cp = find_content_protection(adaptation_set, PLAYREADY_SCHEME);
    (child_text(cp, "pssh"), child_text(cp, "pro"))
}

fn widevine_pssh_text(adaptation_set: &Element) -> Option<&str> {
    child_text(
        find_content_protection(adaptation_set, WIDEVINE_SCHEME),
        "pssh",
    )
}

type SegmentTemplateAttrs<'a> = [Option<&'a str>; 5];

fn segment_template_attrs(adaptation_set: &Element) -> Vec<SegmentTemplateAttrs<'_>> {
    adaptation_set
        .iter_local("SegmentTemplate")
        .into_iter()
        .map(|st| {
            [
                st.get("timescale"),
                st.get("duration"),
                st.get("media"),
                st.get("initialization"),
                st.get("startNumber"),
            ]
        })
        .collect()
}

/// `_in_scope`: config's scoping — `mimeType` in the allow-list and at least one
/// Representation whose codec starts with the content type's allowed prefixes.
fn in_scope(adaptation_set: &Element) -> bool {
    let Some(mime_type) = adaptation_set.get("mimeType") else {
        return false;
    };
    if !IN_SCOPE_MIME_TYPES.contains(&mime_type) {
        return false;
    }
    let allowed = in_scope_codec_prefixes(adaptation_set.get("contentType").unwrap_or(""));
    if allowed.is_empty() {
        return false;
    }
    adaptation_set
        .iter_local("Representation")
        .into_iter()
        .any(|rep| {
            let codecs = rep.get("codecs").unwrap_or("");
            allowed.iter().any(|prefix| codecs.starts_with(prefix))
        })
}

fn reject_doctype(text: &str) -> Result<(), DashError> {
    if text.contains("<!DOCTYPE") {
        return rewrite_error(
            "DOCTYPE is not permitted in a DASH manifest (entity-expansion DoS guard)",
        );
    }
    Ok(())
}

fn parse_fragment(block: &str) -> Result<Element, DashError> {
    xml::fromstring(block)
        .or_else(|err| rewrite_error(format!("AdaptationSet is not well-formed XML: {err}")))
}

fn parse_manifest(text: &str) -> Result<Element, DashError> {
    xml::fromstring(text)
        .or_else(|err| rewrite_error(format!("manifest is not well-formed XML: {err}")))
}

/// `_count_adaptation_sets`: the namespace-safe count from a whole-document parse.
fn count_adaptation_sets(text: &str) -> Result<usize, DashError> {
    Ok(parse_manifest(text)?.iter_local("AdaptationSet").len())
}

// ------------------------------------------------------------------------------------------
// The text-surgical scan (`_ADAPTATION_SET_RE`, `_ADAPTATION_SET_OPEN_RE`, `_BASEURL_RE`,
// `_MPD_OPEN_RE`)
// ------------------------------------------------------------------------------------------

/// `_ADAPTATION_SET_RE.split(text)`: `[tissue, block, tissue, …, tissue]`, a block being
/// `<AdaptationSet\b` through the first `</AdaptationSet>` after it.
fn split_adaptation_sets(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut last = 0;
    let mut search = 0;
    while let Some(found) = text[search..].find(ADAPTATION_SET_OPEN) {
        let start = search + found;
        let after = start + ADAPTATION_SET_OPEN.len();
        if !py::word_boundary_after_word(text, after) {
            search = start + 1;
            continue;
        }
        let Some(offset) = text[after..].find(ADAPTATION_SET_CLOSE) else {
            break;
        };
        let end = after + offset + ADAPTATION_SET_CLOSE.len();
        parts.push(&text[last..start]);
        parts.push(&text[start..end]);
        last = end;
        search = end;
    }
    parts.push(&text[last..]);
    parts
}

/// `len(_ADAPTATION_SET_OPEN_RE.findall(text))`.
fn count_adaptation_set_opens(text: &str) -> usize {
    text.match_indices(ADAPTATION_SET_OPEN)
        .filter(|(start, _)| py::word_boundary_after_word(text, start + ADAPTATION_SET_OPEN.len()))
        .count()
}

/// The first match of `<tag\b[^>]*>`: the offset after its `>`.
fn find_open_tag(text: &str, open: &str) -> Option<usize> {
    let mut search = 0;
    while let Some(found) = text[search..].find(open) {
        let start = search + found;
        let after = start + open.len();
        if py::word_boundary_after_word(text, after) {
            return text[after..].find('>').map(|offset| after + offset + 1);
        }
        search = start + 1;
    }
    None
}

/// `_BASEURL_RE.search(text)`: the span of group 1 of `<BaseURL\b[^>]*>(.*?)</BaseURL>`.
fn find_base_url(text: &str) -> Option<(usize, usize)> {
    let content_start = find_open_tag(text, "<BaseURL")?;
    let length = text[content_start..].find("</BaseURL>")?;
    Some((content_start, content_start + length))
}

fn replace_exactly_once(
    text: &str,
    old: &str,
    new: &str,
    context: &str,
) -> Result<String, DashError> {
    let count = text.matches(old).count();
    if count != 1 {
        return rewrite_error(format!(
            "expected exactly one {context} in the AdaptationSet block, found {count} \
             (unmodeled manifest shape -- refusing to guess)"
        ));
    }
    Ok(text.replacen(old, new, 1))
}

/// `_patch_in_scope_block`: the scheme value, then the `PlayReady` pair, in the block's text.
fn patch_in_scope_block(
    block: &str,
    adaptation_set: &Element,
    playready: &PlayReadyObjects,
) -> Result<String, DashError> {
    let mut edited = block.to_string();
    let Some(mp4protection) = find_content_protection(adaptation_set, MP4PROTECTION_SCHEME) else {
        return rewrite_error(
            "in-scope AdaptationSet has no urn:mpeg:dash:mp4protection:2011 ContentProtection",
        );
    };
    match mp4protection.get("value") {
        Some("cenc") => {
            edited = replace_exactly_once(
                &edited,
                "value=\"cenc\"",
                "value=\"cbcs\"",
                "mp4protection value=\"cenc\"",
            )?;
        }
        Some("cbcs") => {}
        value => {
            return rewrite_error(format!(
                "unexpected mp4protection @value {} (expected 'cenc' or 'cbcs')",
                repr_opt(value)
            ));
        }
    }
    if let Some(cp) = find_content_protection(adaptation_set, PLAYREADY_SCHEME) {
        let old_pssh = child_text(Some(cp), "pssh").filter(|text| !text.is_empty());
        let old_pro = child_text(Some(cp), "pro").filter(|text| !text.is_empty());
        let (Some(old_pssh), Some(old_pro)) = (old_pssh, old_pro) else {
            return rewrite_error(
                "PlayReady ContentProtection is missing its <cenc:pssh> or <mspr:pro>",
            );
        };
        edited = replace_exactly_once(
            &edited,
            old_pssh,
            &playready.pssh_b64,
            "PlayReady <cenc:pssh>",
        )?;
        edited =
            replace_exactly_once(&edited, old_pro, &playready.pro_b64, "PlayReady <mspr:pro>")?;
    }
    Ok(edited)
}

/// `_apply_url_base`: the MPD-level `<BaseURL>` repointed (or one injected after `<MPD …>`),
/// `url_base` XML-escaped (`&`, `<`, `>`).
fn apply_url_base(text: &str, url_base: &str) -> Result<String, DashError> {
    let escaped = xml::escape_cdata(url_base);
    if let Some((start, end)) = find_base_url(text) {
        return Ok(format!("{}{escaped}{}", &text[..start], &text[end..]));
    }
    let Some(mpd_end) = find_open_tag(text, "<MPD") else {
        return rewrite_error("cannot apply url_base: no <MPD> element found");
    };
    Ok(format!(
        "{}<BaseURL>{escaped}</BaseURL>{}",
        &text[..mpd_end],
        &text[mpd_end..]
    ))
}

/// `localize_dash_manifest_http`: the serve-time BaseURL-only repoint of a manifest's text;
/// `resolve_base_url` gets the manifest's own `<BaseURL>` text. No DRM patching, no disk I/O.
pub fn localize_dash_manifest_http(
    mpd_text: &str,
    resolve_base_url: impl FnOnce(&str) -> String,
) -> Result<String, DashError> {
    reject_doctype(mpd_text)?;
    let Some((start, end)) = find_base_url(mpd_text) else {
        return rewrite_error("cannot localize: no <BaseURL> found in the manifest");
    };
    let target = resolve_base_url(&mpd_text[start..end]);
    apply_url_base(mpd_text, &target)
}

/// `parse_source_playready`: the existing manifest's `<mspr:pro>`, parsed (KID + `LA_URL`).
pub fn parse_source_playready(mpd_path: &Path) -> Result<ParsedPro, DashError> {
    let text = py::read_utf8(mpd_path)?;
    reject_doctype(&text)?;
    let root = parse_manifest(&text)?;
    let Some(cp) = find_content_protection(&root, PLAYREADY_SCHEME) else {
        return rewrite_error("no PlayReady ContentProtection found in the manifest");
    };
    let pro_b64 = child_text(Some(cp), "pro").unwrap_or("");
    if py::strip(pro_b64).is_empty() {
        return rewrite_error("PlayReady ContentProtection has no <mspr:pro> to parse");
    }
    Ok(parse_pro(py::strip(pro_b64))?)
}

/// `rewrite_dash_manifest`: patch `source_mpd` into `output_mpd`. `playready` must be the same
/// objects the `--pssh` init path consumes. `url_base`: `None` is the deploy form,
/// a string the local form (the MPD-level `<BaseURL>` repointed). The input is read-only.
pub fn rewrite_dash_manifest(
    source_mpd: &Path,
    output_mpd: &Path,
    playready: &PlayReadyObjects,
    url_base: Option<&str>,
) -> Result<DashRewriteResult, DashError> {
    let source_text = py::read_utf8(source_mpd)?;
    reject_doctype(&source_text)?;
    let mut parts: Vec<String> = split_adaptation_sets(&source_text)
        .into_iter()
        .map(str::to_string)
        .collect();
    let blocks = parts.len() / 2;
    let opens = count_adaptation_set_opens(&source_text);
    if blocks != opens {
        return rewrite_error(format!(
            "AdaptationSet block split captured {blocks} of {opens} opening tags \
             (a block boundary was not cleanly matched -- refusing to edit)"
        ));
    }
    let parsed = count_adaptation_sets(&source_text)?;
    if blocks != parsed {
        return rewrite_error(format!(
            "text-surgical scan matched {blocks} <AdaptationSet> block(s) but the parsed \
             manifest contains {parsed} AdaptationSet element(s) -- the scan matches only \
             unprefixed <AdaptationSet> tags, so a namespace-prefixed manifest (e.g. \
             <mpd:AdaptationSet>) would be silently skipped; refusing to write an unpatched \
             manifest that would falsely report PASS"
        ));
    }
    let mut block_pairs: Vec<(String, String, bool)> = Vec::new();
    for index in (1..parts.len()).step_by(2) {
        let block = parts[index].clone();
        let adaptation_set = parse_fragment(&block)?;
        if in_scope(&adaptation_set) {
            let edited = patch_in_scope_block(&block, &adaptation_set, playready)?;
            parts[index].clone_from(&edited);
            block_pairs.push((block, edited, true));
        } else {
            block_pairs.push((block.clone(), block, false));
        }
    }
    let mut output_text = parts.concat();
    if let Some(url_base) = url_base {
        output_text = apply_url_base(&output_text, url_base)?;
    }
    py::mkdir_parents(&parent_dir(output_mpd)?)?;
    py::write_bytes(output_mpd, output_text.as_bytes())?;
    let (checks, widevine_unchanged) = compute_checks(&block_pairs, playready)?;
    Ok(DashRewriteResult {
        output_mpd_path: output_mpd.to_path_buf(),
        adaptation_sets_patched: block_pairs.iter().filter(|(_, _, scope)| *scope).count(),
        playready_pssh_b64: playready.pssh_b64.clone(),
        playready_pro_b64: playready.pro_b64.clone(),
        widevine_unchanged,
        checks,
    })
}

/// `path.parent` as a path (`mkdir(parents=True, exist_ok=True)` target).
fn parent_dir(file: &Path) -> Result<PathBuf, DashError> {
    Ok(PathBuf::from(path::parent(&path::fspath(file)?)))
}

/// `_compute_checks`: re-parse the written blocks against the source, per checklist anchor.
fn compute_checks(
    block_pairs: &[(String, String, bool)],
    playready: &PlayReadyObjects,
) -> Result<(Vec<(CheckId, String)>, bool), DashError> {
    let (mut scheme_ok, mut kid_ok, mut pr_ok, mut wv_ok, mut segtpl_ok, mut oos_ok) =
        (true, true, true, true, true, true);
    let mut pr_seen = false;
    for (source_block, output_block, scoped) in block_pairs {
        let source = parse_fragment(source_block)?;
        let output = parse_fragment(output_block)?;
        if widevine_pssh_text(&output) != widevine_pssh_text(&source) {
            wv_ok = false;
        }
        if *scoped {
            if mp4protection_value(&output) != Some("cbcs") {
                scheme_ok = false;
            }
            if default_kid(&output) != default_kid(&source) {
                kid_ok = false;
            }
            let (source_pssh, source_pro) = playready_texts(&source);
            let (output_pssh, output_pro) = playready_texts(&output);
            if source_pssh.is_some() || source_pro.is_some() {
                pr_seen = true;
                if output_pssh != Some(playready.pssh_b64.as_str())
                    || output_pro != Some(playready.pro_b64.as_str())
                {
                    pr_ok = false;
                }
            }
            if segment_template_attrs(&output) != segment_template_attrs(&source) {
                segtpl_ok = false;
            }
        } else if output_block != source_block {
            oos_ok = false;
        }
    }
    let playready_status = if !pr_seen {
        "SKIP: no PlayReady ContentProtection in in-scope AdaptationSets"
    } else if pr_ok {
        "PASS"
    } else {
        "FAIL: PlayReady <cenc:pssh>/<mspr:pro> not in sync with the generated bytes"
    };
    let pass_or = |ok: bool, failure: &str| {
        if ok {
            "PASS".to_string()
        } else {
            failure.to_string()
        }
    };
    let scheme_status = if scheme_ok && kid_ok {
        "PASS".to_string()
    } else if !scheme_ok {
        "FAIL: scheme not cbcs".to_string()
    } else {
        "FAIL: default_KID changed".to_string()
    };
    let checks = vec![
        (CheckId::DashSchemeCbcs, scheme_status),
        (CheckId::DashPlayreadySync, playready_status.to_string()),
        (
            CheckId::DashWidevineUnchanged,
            pass_or(wv_ok, "FAIL: Widevine <cenc:pssh> changed"),
        ),
        (
            CheckId::DashSegmenttemplateUnchanged,
            pass_or(segtpl_ok, "FAIL: SegmentTemplate attribute changed"),
        ),
        (
            CheckId::DashOutOfScopeUntouched,
            pass_or(oos_ok, "FAIL: out-of-scope AdaptationSet changed"),
        ),
    ];
    Ok((checks, wv_ok))
}

// ------------------------------------------------------------------------------------------
// analyze_dash_manifest: the inspector's pure-parse summary (read-only parsing)
// ------------------------------------------------------------------------------------------

/// `DashRepresentationSummary`: one inspector row per `<Representation>`, out-of-scope ones
/// included. Python's ints are unbounded, hence `BigInt` for the parsed attributes.
#[derive(Clone, Debug, PartialEq)]
pub struct DashRepresentationSummary {
    /// `Representation@id`.
    pub rep_id: String,
    /// `AdaptationSet@contentType`.
    pub content_type: String,
    /// `Representation@codecs`.
    pub codec: String,
    /// `AdaptationSet@mimeType`.
    pub mime_type: String,
    /// `fmp4` | `webm` | the mimeType's subtype.
    pub container: String,
    /// config's scoping.
    pub in_scope: bool,
    /// The mp4protection `@value`.
    pub protection_scheme: Option<String>,
    /// `cenc:default_KID` (public).
    pub default_kid: Option<String>,
    /// `PlayReady` / `Widevine`, sorted.
    pub drm_systems: Vec<String>,
    /// From the PRO.
    pub playready_version: Option<String>,
    /// From the PRO.
    pub playready_algid: Option<String>,
    /// `SegmentTemplate@timescale`.
    pub timescale: Option<BigInt>,
    /// `SegmentTemplate@duration`.
    pub segment_duration_ticks: Option<BigInt>,
    /// ticks / timescale.
    pub segment_duration_s: Option<f64>,
    /// `SegmentTemplate@startNumber` (1 when absent).
    pub start_number: Option<BigInt>,
    /// `Representation@bandwidth` when it is all digits.
    pub bandwidth: Option<BigInt>,
}

/// `DashManifestSummary`.
#[derive(Clone, Debug, PartialEq)]
pub struct DashManifestSummary {
    /// One row per Representation, in document order.
    pub representations: Vec<DashRepresentationSummary>,
}

fn required_attr<'a>(
    element: &'a Element,
    name: &str,
    mpd_path: &str,
) -> Result<&'a str, DashError> {
    match element.get(name) {
        Some(value) => Ok(value),
        None => rewrite_error(format!(
            "{mpd_path}: <{}> missing required @{name}",
            element.tag
        )),
    }
}

fn container_for_mime_type(mime_type: &str) -> String {
    match mime_type {
        "video/mp4" | "audio/mp4" => "fmp4".to_string(),
        "video/webm" => "webm".to_string(),
        _ => mime_type
            .rsplit('/')
            .next()
            .unwrap_or(mime_type)
            .to_string(),
    }
}

/// `_drm_systems`: the known DRM systems present, sorted.
fn drm_systems(adaptation_set: &Element) -> Vec<String> {
    let mut names: Vec<String> = [
        (PLAYREADY_SCHEME, "PlayReady"),
        (WIDEVINE_SCHEME, "Widevine"),
    ]
    .iter()
    .filter(|(scheme, _)| find_content_protection(adaptation_set, scheme).is_some())
    .map(|(_, name)| (*name).to_string())
    .collect();
    names.sort();
    names
}

/// `_playready_version_algid`: lenient — a PRO that fails to parse (`PlayReadyError`) is
/// `(None, None)`; any other exception from `parse_pro` propagates.
fn playready_version_algid(
    adaptation_set: &Element,
) -> Result<(Option<String>, Option<String>), DashError> {
    let (_, pro_b64) = playready_texts(adaptation_set);
    let Some(pro_b64) = pro_b64.filter(|pro| !py::strip(pro).is_empty()) else {
        return Ok((None, None));
    };
    match parse_pro(py::strip(pro_b64)) {
        Ok(parsed) => Ok((Some(parsed.version), parsed.algid)),
        Err(ParseProError::PlayReady(_)) => Ok((None, None)),
        Err(err @ ParseProError::UnicodeDecode(_)) => Err(err.into()),
    }
}

/// `_representation_segment_info`: `(timescale, duration, startNumber)` of the first
/// `SegmentTemplate` under the Representation.
/// `(timescale, segment_duration_ticks, start_number)`.
type SegmentInfo = (Option<BigInt>, Option<BigInt>, Option<BigInt>);

fn representation_segment_info(rep: &Element) -> Result<SegmentInfo, DashError> {
    let Some(template) = rep.iter_local("SegmentTemplate").into_iter().next() else {
        return Ok((None, None, None));
    };
    let timescale = template.get("timescale").map(py::parse_int).transpose()?;
    let duration = template.get("duration").map(py::parse_int).transpose()?;
    let start_number = py::parse_int(template.get("startNumber").unwrap_or("1"))?;
    Ok((timescale, duration, Some(start_number)))
}

/// `analyze_dash_manifest`: the inspector's summary of one manifest file; reads nothing else.
pub fn analyze_dash_manifest(mpd_path: &Path) -> Result<DashManifestSummary, DashError> {
    let display_path = path::pure(&path::fspath(mpd_path)?);
    let text = py::read_utf8(mpd_path)?;
    reject_doctype(&text)?;
    let root = parse_manifest(&text)?;
    let mut representations = Vec::new();
    for adaptation_set in root.iter_local("AdaptationSet") {
        let content_type = required_attr(adaptation_set, "contentType", &display_path)?;
        let mime_type = required_attr(adaptation_set, "mimeType", &display_path)?;
        let scoped = in_scope(adaptation_set);
        let protection_scheme = mp4protection_value(adaptation_set).map(str::to_string);
        let kid = default_kid(adaptation_set).map(str::to_string);
        let systems = drm_systems(adaptation_set);
        let (playready_version, playready_algid) = playready_version_algid(adaptation_set)?;
        for rep in adaptation_set.iter_local("Representation") {
            let rep_id = required_attr(rep, "id", &display_path)?;
            let codec = required_attr(rep, "codecs", &display_path)?;
            let (timescale, segment_duration_ticks, start_number) =
                representation_segment_info(rep)?;
            let segment_duration_s = match (&segment_duration_ticks, &timescale) {
                (Some(ticks), Some(scale)) if py::truthy(scale) => {
                    Some(Fraction::new(ticks.clone(), scale.clone())?.to_f64()?)
                }
                _ => None,
            };
            let bandwidth_text = rep.get("bandwidth").unwrap_or("");
            let bandwidth = if py::isdigit(bandwidth_text) {
                Some(py::parse_int(bandwidth_text)?)
            } else {
                None
            };
            representations.push(DashRepresentationSummary {
                rep_id: rep_id.to_string(),
                content_type: content_type.to_string(),
                codec: codec.to_string(),
                mime_type: mime_type.to_string(),
                container: container_for_mime_type(mime_type),
                in_scope: scoped,
                protection_scheme: protection_scheme.clone(),
                default_kid: kid.clone(),
                drm_systems: systems.clone(),
                playready_version: playready_version.clone(),
                playready_algid: playready_algid.clone(),
                timescale,
                segment_duration_ticks,
                segment_duration_s,
                start_number,
                bandwidth,
            });
        }
    }
    Ok(DashManifestSummary { representations })
}
