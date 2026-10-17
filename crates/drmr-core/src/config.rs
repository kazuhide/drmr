//! Asset configuration, dynamic rendition discovery, and protected key material.
//!
//! Manifest text is untrusted: document type declarations are rejected, derived paths
//! are confined to the download root, and path components are validated. Keys and IVs
//! come from the environment or explicit mappings, never CLI flags. Their debug output
//! hides values, and memory holding keys and IVs is wiped on drop.

use crate::pycompat::{
    GlobError, PurePosixPath, ResolveError, b64decode, etree_get, etree_parse, etree_text, fromhex,
    glob, int, path_str, repr_str, resolve, strip, urljoin, urlparse, xml_line_ends,
};
use regex::Regex;
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use zeroize::Zeroizing;

/// The DASH MPD namespace.
pub const MPD_NS: &str = "urn:mpeg:dash:schema:mpd:2011";
/// The Common Encryption namespace (`cenc:pssh`, `cenc:default_KID`).
pub const CENC_NS: &str = "urn:mpeg:cenc:2013";
/// The Widevine `ContentProtection@schemeIdUri`.
pub const WIDEVINE_SYSTEM_ID_URI: &str = "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";

/// Only H.264 / H.265 video and AAC audio are in scope : this allow-list is what
/// excludes VP9 (`video/webm`) and every subtitle or text `AdaptationSet`.
pub const IN_SCOPE_MIME_TYPES: [&str; 2] = ["video/mp4", "audio/mp4"];

/// The codec prefixes in scope per content type (`_IN_SCOPE_CODEC_PREFIXES`): the MIME type
/// alone is not precise enough (an AV1 or EC-3 rendition in an MP4 container passes it).
pub fn in_scope_codec_prefixes(content_type: &str) -> &'static [&'static str] {
    match content_type {
        "video" => &["avc1", "hvc1"],
        "audio" => &["mp4a"],
        _ => &[],
    }
}

/// An adaptation can be rewritten only when all of its representations share codec scope.
pub(crate) fn adaptation_codec_scope<'a>(
    content_type: &str,
    codecs: impl IntoIterator<Item = &'a str>,
) -> Result<bool, &'static str> {
    let allowed = in_scope_codec_prefixes(content_type);
    let (mut supported, mut unsupported) = (false, false);
    for codec in codecs {
        if allowed.iter().any(|prefix| codec.starts_with(prefix)) {
            supported = true;
        } else {
            unsupported = true;
        }
    }
    if supported && unsupported {
        Err("mixed supported and unsupported codecs in one AdaptationSet")
    } else {
        Ok(supported)
    }
}

/// The first run of digits in a segment file stem (Python's `\d` matches every Unicode decimal
/// digit, as Rust's does).
static SEGMENT_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\d+)").expect("a fixed pattern compiles"));

/// A configuration failure, by the Python exception class it stands for.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// `ValueError` (including `binascii.Error`), with a compatible message.
    #[error("{0}")]
    Value(String),
    /// `xml.etree.ElementTree.ParseError`: the manifest is not well-formed XML (or not UTF-8).
    #[error("{0}")]
    Xml(String),
    /// `RuntimeError`: `Path.resolve` met a symbolic-link loop.
    #[error("{0}")]
    Runtime(String),
    /// `ZeroDivisionError`: a zero timescale.
    #[error("division by zero")]
    ZeroDivision,
    /// `OSError`.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<ResolveError> for ConfigError {
    fn from(error: ResolveError) -> Self {
        match error {
            ResolveError::SymlinkLoop(_) => ConfigError::Runtime(error.to_string()),
            ResolveError::Io(error) => ConfigError::Io(error),
        }
    }
}

impl From<GlobError> for ConfigError {
    fn from(error: GlobError) -> Self {
        match error {
            GlobError::Value(message) => ConfigError::Value(message),
            GlobError::NotRelative => ConfigError::Value(error.to_string()),
            GlobError::Io(error) => ConfigError::Io(error),
        }
    }
}

// ------------------------------------------------------------------------------------------
// Key material
// ------------------------------------------------------------------------------------------

/// The KID, content key and IV as hex text (`KeyMaterial`), each validated as 16 bytes of hex
/// on construction. Loaded from the environment or a gitignored config, never logged,
/// committed or put on the tool's own command line.
///
/// `Debug` prints no value (the type name only); the key and the IV are wiped from memory when
/// the value is dropped. The KID is public (`cenc:default_KID`) but is not printed
/// either. The texts are kept as given (case, embedded whitespace), as Python keeps them.
#[derive(Clone, PartialEq, Eq)]
#[allow(clippy::struct_field_names)] // reason: Python's field names (key_id_hex, key_hex, iv_hex)
pub struct KeyMaterial {
    key_id_hex: String,
    key_hex: Zeroizing<String>,
    iv_hex: Zeroizing<String>,
}

impl fmt::Debug for KeyMaterial {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyMaterial").finish_non_exhaustive()
    }
}

/// `bytes.fromhex(value)` must work and give 16 bytes.
fn require_16_byte_hex(value: &str, field_name: &str) -> Result<(), ConfigError> {
    let raw = Zeroizing::new(
        fromhex(value)
            .map_err(|_| ConfigError::Value(format!("{field_name} must be a hex string")))?,
    );
    if raw.len() != 16 {
        return Err(ConfigError::Value(format!(
            "{field_name} must decode to 16 bytes, got {}",
            raw.len()
        )));
    }
    Ok(())
}

impl KeyMaterial {
    /// `KeyMaterial(key_id_hex=..., key_hex=..., iv_hex=...)`: each must be hex for 16 bytes,
    /// checked in that order (The classified error messages: `<field> must be a hex string`, `<field> must
    /// decode to 16 bytes, got N`).
    pub fn new(
        key_id_hex: impl Into<String>,
        key_hex: impl Into<String>,
        iv_hex: impl Into<String>,
    ) -> Result<Self, ConfigError> {
        let material = KeyMaterial {
            key_id_hex: key_id_hex.into(),
            key_hex: Zeroizing::new(key_hex.into()),
            iv_hex: Zeroizing::new(iv_hex.into()),
        };
        require_16_byte_hex(&material.key_id_hex, "key_id_hex")?;
        require_16_byte_hex(&material.key_hex, "key_hex")?;
        require_16_byte_hex(&material.iv_hex, "iv_hex")?;
        Ok(material)
    }

    /// The KID (unchanged across CENC and CBCS,; public).
    pub fn key_id_hex(&self) -> &str {
        &self.key_id_hex
    }

    /// The content key (unchanged). Secret: never log, print or embed it.
    pub fn key_hex(&self) -> &str {
        &self.key_hex
    }

    /// The 16-byte constant IV of the CBCS output (passed explicitly as `--iv`). Secret.
    pub fn iv_hex(&self) -> &str {
        &self.iv_hex
    }
}

/// `load_key_material(env)`: `DRM_KID` / `DRM_KEY` / `DRM_IV` from `env`, or from the process
/// environment when `env` is `None` (the same configuration keeps one KID, key and IV across
/// CENC and CBCS). An absent or empty variable is missing; every missing one is named.
#[allow(clippy::implicit_hasher)] // reason: a generic hasher would make `load_key_material(None)` uninferable
pub fn load_key_material(
    env: Option<&HashMap<String, String>>,
) -> Result<KeyMaterial, ConfigError> {
    const NAMES: [&str; 3] = ["DRM_KID", "DRM_KEY", "DRM_IV"];
    let lookup = |name: &str| -> Option<Zeroizing<String>> {
        match env {
            Some(values) => values.get(name).cloned().map(Zeroizing::new),
            // Python reads os.environ, where an undecodable value is a str that fails the hex
            // check; the lossy text fails it the same way.
            None => std::env::var_os(name)
                .map(|value| Zeroizing::new(value.to_string_lossy().into_owned())),
        }
    };
    let mut values: Vec<Option<Zeroizing<String>>> =
        NAMES.iter().map(|name| lookup(name)).collect();
    let missing: Vec<&str> = NAMES
        .iter()
        .zip(&values)
        .filter(|(_, value)| value.as_ref().is_none_or(|value| value.is_empty()))
        .map(|(name, _)| *name)
        .collect();
    if !missing.is_empty() {
        return Err(ConfigError::Value(format!(
            "missing required env var(s): {}",
            missing.join(", ")
        )));
    }
    let mut take = |index: usize| values[index].take().expect("checked present");
    let (kid, key, iv) = (take(0), take(1), take(2));
    KeyMaterial::new(kid.as_str(), key.as_str(), iv.as_str())
}

// ------------------------------------------------------------------------------------------
// Renditions
// ------------------------------------------------------------------------------------------

/// One in-scope rendition of the source manifest (`Rendition`, with all discovered fields).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rendition {
    /// DASH `Representation@id` (unchanged).
    pub rep_id: String,
    /// `video` or `audio`.
    pub content_type: String,
    /// The codec string (`avc1.42C00C`, `hvc1.1.6.L60.90`, `mp4a.40.2`).
    pub codec: String,
    /// `SegmentTemplate@timescale`.
    pub timescale: i64,
    /// `SegmentTemplate@duration`, in timescale ticks.
    pub segment_duration_ticks: i64,
    /// `SegmentTemplate@startNumber` (default 1; 0 on the source presentation).
    pub start_number: i64,
    /// The resolved, read-only source init segment.
    pub init_path: PathBuf,
    /// The source segments in NUMERIC order (`seg_2` before `seg_10`).
    pub segment_paths: Vec<PathBuf>,
    /// The source-relative directory (`video/<id>`) mirrored under `output/`.
    pub rel_dir: PathBuf,
    /// The Widevine `<cenc:pssh>` as hex, copied opaquely; `None` when absent.
    pub widevine_pssh_hex: Option<String>,
    /// `Representation@width` (video only).
    pub width: Option<i64>,
    /// `Representation@height` (video only).
    pub height: Option<i64>,
    /// `Representation@frameRate` (video only), as written.
    pub frame_rate: Option<String>,
    /// `AdaptationSet@lang`.
    pub lang: Option<String>,
}

/// Source-derived segment duration in seconds, computed as ticks / timescale.
/// A zero timescale is a division error; no hardcoded duration is substituted.
pub fn segment_duration_seconds(rendition: &Rendition) -> Result<f64, ConfigError> {
    if rendition.timescale == 0 {
        return Err(ConfigError::ZeroDivision);
    }
    // Python's int / int is correctly rounded; so is this division while both values are exact
    // doubles (|v| <= 2**53, every real manifest). Beyond that the result may differ in the
    // last bit (documented limit).
    #[allow(clippy::cast_precision_loss)] // reason: exact for |v| <= 2**53, see above
    let (ticks, timescale) = (
        rendition.segment_duration_ticks as f64,
        rendition.timescale as f64,
    );
    Ok(ticks / timescale)
}

/// The `download` anchor directory above `mpd_path` (`_find_download_root`): the downloader
/// mirrors every fetched URL into `download/<netloc><path>`, so the manifest and its segments
/// share that root under different hosts, and segment paths resolve through it.
pub fn find_download_root(mpd_path: &Path) -> Result<PathBuf, ConfigError> {
    let resolved = resolve(mpd_path)?;
    for ancestor in resolved.ancestors().skip(1) {
        if ancestor.file_name().is_some_and(|name| name == "download") {
            return Ok(ancestor.to_path_buf());
        }
    }
    Err(ConfigError::Value(format!(
        "{} is not nested under a 'download' directory (expected the URL-to-file \
         mirroring convention: download/<host>/<path>/...)",
        path_str(mpd_path)
    )))
}

/// A manifest-relative media or init URL as a local path under `download_root`
/// (`_resolve_local_path`), mirroring the downloader's netloc + path convention.
///
/// SECURITY: the manifest is untrusted. `urljoin` neutralizes a plain `../` in the URL path
/// but not a `..` in the authority (`https://../../x`), so the result is confined explicitly:
/// a path that resolves outside the download root is an error.
pub fn resolve_local_path(
    download_root: &Path,
    base_url: &str,
    relative_url: &str,
) -> Result<PathBuf, ConfigError> {
    let full_url = urljoin(base_url, relative_url).map_err(ConfigError::Value)?;
    let parsed = urlparse(&full_url, "").map_err(ConfigError::Value)?;
    let joined = PurePosixPath::from_path(download_root)
        .join(&parsed.netloc)
        .join(parsed.path.trim_start_matches('/'));
    let resolved = resolve(&joined.to_path_buf())?;
    let root = resolve(download_root)?;
    if resolved != root && !resolved.starts_with(&root) {
        return Err(ConfigError::Value(format!(
            "manifest-derived path escapes the download root: {} (resolved outside {})",
            repr_str(relative_url),
            path_str(&root)
        )));
    }
    Ok(resolved)
}

/// A manifest-derived string used as ONE path component (`Representation@id`, the derived
/// asset id) must not act as a separator or a traversal once joined onto a confined root.
fn require_safe_path_component<'a>(
    value: &'a str,
    field_name: &str,
    mpd_path: &Path,
) -> Result<&'a str, ConfigError> {
    if value.is_empty() || value == "." || value == ".." || value.contains(['/', '\\', '\0']) {
        return Err(ConfigError::Value(format!(
            "{}: unsafe value for {field_name}: {}",
            path_str(mpd_path),
            repr_str(value)
        )));
    }
    Ok(value)
}

/// A manifest-derived relative directory (the `SegmentTemplate@media` directory) must not
/// escape a confined root when joined: no absolute path, no `..` / `.` / empty segment, not
/// empty itself.
fn require_safe_relative_path(
    value: &PurePosixPath,
    field_name: &str,
    mpd_path: &Path,
) -> Result<PathBuf, ConfigError> {
    if value.is_absolute() {
        return Err(ConfigError::Value(format!(
            "{}: {field_name} must not be absolute: {}",
            path_str(mpd_path),
            value.repr()
        )));
    }
    let parts = value.parts();
    if parts.is_empty() || parts.iter().any(|part| matches!(*part, ".." | "." | "")) {
        return Err(ConfigError::Value(format!(
            "{}: unsafe path segments in {field_name}: {}",
            path_str(mpd_path),
            value.repr()
        )));
    }
    Ok(value.to_path_buf())
}

/// A segment's integer index from the first digit run of its stem, so `seg_2` sorts before
/// `seg_10` (`ls -V` semantics without a shell).
fn numeric_segment_key(path: &Path) -> Result<i64, ConfigError> {
    let stem_owner = PurePosixPath::from_path(path);
    let found = SEGMENT_NUMBER.find(stem_owner.stem()).ok_or_else(|| {
        ConfigError::Value(format!(
            "segment file {} has no numeric index",
            path_str(path)
        ))
    })?;
    int(found.as_str()).map_err(ConfigError::Value)
}

/// `element.get(name)`, required: The classified error message names the element in Clark notation.
fn required_attr<'a>(
    element: roxmltree::Node<'a, '_>,
    name: &str,
    mpd_path: &Path,
) -> Result<&'a str, ConfigError> {
    etree_get(element, name).ok_or_else(|| {
        let tag = element.tag_name();
        let clark = match tag.namespace() {
            Some(namespace) => format!("{{{namespace}}}{}", tag.name()),
            None => tag.name().to_string(),
        };
        ConfigError::Value(format!(
            "{}: <{clark}> missing required @{name}",
            path_str(mpd_path)
        ))
    })
}

/// The manifest text, a DOCTYPE rejected first.
///
/// SECURITY: Python's `ElementTree` expands internal entities (a "billion laughs" manifest
/// expands thousands of times in memory) and a legitimate MPD never has a DOCTYPE, so one is
/// rejected outright — scanning the WHOLE file, so a DOCTYPE pushed past a fixed-size head by
/// a large leading comment cannot slip through.
fn read_mpd(mpd_path: &Path) -> Result<String, ConfigError> {
    let content = std::fs::read(mpd_path)?;
    if content.windows(9).any(|window| window == b"<!DOCTYPE") {
        return Err(ConfigError::Value(format!(
            "{}: DOCTYPE is not permitted in a DASH manifest",
            path_str(mpd_path)
        )));
    }
    // Every real MPD is UTF-8; `expat` would also read the other encodings an XML declaration
    // names (documented limit).
    let text = String::from_utf8(content).map_err(|error| {
        ConfigError::Xml(format!(
            "not well-formed (invalid token): byte {}",
            error.utf8_error().valid_up_to()
        ))
    })?;
    // Line ends as an XML processor reads them (`\r\n` and a lone `\r` are `\n`).
    Ok(match xml_line_ends(&text) {
        std::borrow::Cow::Borrowed(_) => text,
        std::borrow::Cow::Owned(normalized) => normalized,
    })
}

/// `ET.parse`: expat's well-formedness checks, any nesting depth up to the supported one
/// (`pycompat::etree_parse`); a failure is Python's `ParseError` text.
fn parse_mpd(text: &str) -> Result<roxmltree::Document<'_>, ConfigError> {
    etree_parse(text).map_err(ConfigError::Xml)
}

fn is_mpd(node: &roxmltree::Node<'_, '_>, name: &str) -> bool {
    node.is_element()
        && node.tag_name().namespace() == Some(MPD_NS)
        && node.tag_name().name() == name
}

fn mpd_child<'a, 'input>(
    node: roxmltree::Node<'a, 'input>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'input>> {
    node.children().find(|child| is_mpd(child, name))
}

/// The manifest's `<BaseURL>` (a child of the root), stripped; required to resolve segments.
fn base_url(root: roxmltree::Node<'_, '_>, mpd_path: &Path) -> Result<String, ConfigError> {
    let text = mpd_child(root, "BaseURL")
        .map(etree_text)
        .unwrap_or_default();
    let stripped = strip(&text);
    if stripped.is_empty() {
        return Err(ConfigError::Value(format!(
            "{}: no <BaseURL> found (required to resolve segment paths)",
            path_str(mpd_path)
        )));
    }
    Ok(stripped.to_string())
}

/// The first Widevine `<cenc:pssh>` of an `AdaptationSet`, as hex (`base64.b64decode`, the
/// lenient decoder).
fn widevine_pssh_hex(
    adaptation_set: roxmltree::Node<'_, '_>,
) -> Result<Option<String>, ConfigError> {
    for protection in adaptation_set
        .children()
        .filter(|child| is_mpd(child, "ContentProtection"))
    {
        if etree_get(protection, "schemeIdUri") != Some(WIDEVINE_SYSTEM_ID_URI) {
            continue;
        }
        let pssh = protection.children().find(|child| {
            child.is_element()
                && child.tag_name().namespace() == Some(CENC_NS)
                && child.tag_name().name() == "pssh"
        });
        if let Some(pssh) = pssh {
            let text = etree_text(pssh);
            let stripped = strip(&text);
            if !stripped.is_empty() {
                let data = b64decode(stripped, false).map_err(ConfigError::Value)?;
                return Ok(Some(hex::encode(data)));
            }
        }
    }
    Ok(None)
}

/// `int(...)` of an attribute: Python's `ValueError` message on failure.
fn int_attr(value: &str) -> Result<i64, ConfigError> {
    int(value).map_err(ConfigError::Value)
}

/// Dynamically enumerate the in-scope renditions (H.264 / H.265 video and AAC audio), in
/// manifest order.
///
/// Excludes VP9 (`video/webm`) and every non-MP4 `AdaptationSet`. Never hardcodes a
/// count : the source presentation is 3 + 3 + 2, production 7 video tiers × 2–4 audio.
pub fn enumerate_renditions(mpd_path: &Path) -> Result<Vec<Rendition>, ConfigError> {
    let mpd_path = PurePosixPath::from_path(mpd_path).to_path_buf();
    let text = read_mpd(&mpd_path)?;
    let document = parse_mpd(&text)?;
    let root = document.root_element();
    let base_url = base_url(root, &mpd_path)?;
    let download_root = find_download_root(&mpd_path)?;
    let mut renditions = Vec::new();
    for adaptation_set in root
        .descendants()
        .skip(1)
        .filter(|node| is_mpd(node, "AdaptationSet"))
    {
        if !etree_get(adaptation_set, "mimeType")
            .is_some_and(|mime| IN_SCOPE_MIME_TYPES.contains(&mime))
        {
            continue;
        }
        let content_type = required_attr(adaptation_set, "contentType", &mpd_path)?;
        adaptation_codec_scope(
            content_type,
            adaptation_set
                .children()
                .filter(|node| is_mpd(node, "Representation"))
                .map(|representation| etree_get(representation, "codecs").unwrap_or("")),
        )
        .map_err(|message| ConfigError::Value(message.into()))?;
        let widevine = widevine_pssh_hex(adaptation_set)?;
        for representation in adaptation_set
            .children()
            .filter(|node| is_mpd(node, "Representation"))
        {
            let source = Source {
                mpd_path: &mpd_path,
                download_root: &download_root,
                base_url: &base_url,
                adaptation_set,
                content_type,
                widevine: widevine.as_ref(),
            };
            if let Some(rendition) = source.rendition(representation)? {
                renditions.push(rendition);
            }
        }
    }
    Ok(renditions)
}

/// What every Representation of one `AdaptationSet` shares.
struct Source<'a, 'input> {
    mpd_path: &'a Path,
    download_root: &'a Path,
    base_url: &'a str,
    adaptation_set: roxmltree::Node<'a, 'input>,
    content_type: &'a str,
    widevine: Option<&'a String>,
}

impl Source<'_, '_> {
    /// One Representation as a [`Rendition`], `None` when its codec is out of scope. The
    /// checks run in field insertion order, so the first failure is Python's first failure.
    fn rendition(
        &self,
        representation: roxmltree::Node<'_, '_>,
    ) -> Result<Option<Rendition>, ConfigError> {
        let mpd_path = self.mpd_path;
        let Some(template) = mpd_child(representation, "SegmentTemplate") else {
            return Err(ConfigError::Value(format!(
                "{}: Representation {} has no SegmentTemplate",
                path_str(mpd_path),
                etree_get(representation, "id").unwrap_or("None")
            )));
        };
        let codec = required_attr(representation, "codecs", mpd_path)?;
        if !in_scope_codec_prefixes(self.content_type)
            .iter()
            .any(|prefix| codec.starts_with(prefix))
        {
            return Ok(None); // AV1 and EC-3 are outside the supported MP4 codec scope.
        }
        let media_rel = required_attr(template, "media", mpd_path)?;
        let init_rel = required_attr(template, "initialization", mpd_path)?;
        let init_path = resolve_local_path(self.download_root, self.base_url, init_rel)?;
        let media = PurePosixPath::new(media_rel);
        let segment_glob = media.name().replace("$Number$", "*");
        let segment_dir = PurePosixPath::from_path(&init_path).parent().to_path_buf();
        let mut keyed = Vec::new();
        for path in glob(&segment_dir, &segment_glob)? {
            keyed.push((numeric_segment_key(&path)?, path));
        }
        keyed.sort_by_key(|(key, _)| *key); // stable: equal keys keep the listing order
        let rep_id = require_safe_path_component(
            required_attr(representation, "id", mpd_path)?,
            "Representation/@id",
            mpd_path,
        )?;
        let rel_dir = require_safe_relative_path(
            &media.parent(),
            "SegmentTemplate/@media directory",
            mpd_path,
        )?;
        let video = self.content_type == "video";
        let video_int = |name: &str| -> Result<Option<i64>, ConfigError> {
            match etree_get(representation, name) {
                Some(value) if video && !value.is_empty() => int_attr(value).map(Some),
                _ => Ok(None),
            }
        };
        Ok(Some(Rendition {
            rep_id: rep_id.to_string(),
            content_type: self.content_type.to_string(),
            codec: codec.to_string(),
            timescale: int_attr(required_attr(template, "timescale", mpd_path)?)?,
            segment_duration_ticks: int_attr(required_attr(template, "duration", mpd_path)?)?,
            start_number: int_attr(etree_get(template, "startNumber").unwrap_or("1"))?,
            init_path,
            segment_paths: keyed.into_iter().map(|(_, path)| path).collect(),
            rel_dir,
            widevine_pssh_hex: self.widevine.cloned(),
            width: video_int("width")?,
            height: video_int("height")?,
            frame_rate: etree_get(representation, "frameRate")
                .filter(|_| video)
                .map(str::to_string),
            lang: etree_get(self.adaptation_set, "lang").map(str::to_string),
        }))
    }
}

// ------------------------------------------------------------------------------------------
// Asset
// ------------------------------------------------------------------------------------------

/// One asset (`Asset`): its id, manifest, read-only input root, output directory, key
/// material and in-scope renditions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    /// The `output/<asset_id>/` key: the outer title id.
    pub asset_id: String,
    /// The manifest path as given.
    pub mpd_path: PathBuf,
    /// The read-only `download/` root.
    pub input_base_dir: PathBuf,
    /// `output/<asset_id>/`.
    pub output_dir: PathBuf,
    /// The key material (prints nothing).
    pub key: KeyMaterial,
    /// The in-scope renditions, in manifest order.
    pub renditions: Vec<Rendition>,
}

/// The outer title id: the first segment of the `BaseURL` path — stable per title and shared
/// by the DASH and the HLS delivery manifests.
fn derive_asset_id(mpd_path: &Path) -> Result<String, ConfigError> {
    let text = read_mpd(mpd_path)?;
    let document = parse_mpd(&text)?;
    let base_url = base_url(document.root_element(), mpd_path)?;
    let parsed = urlparse(&base_url, "").map_err(ConfigError::Value)?;
    let first = parsed
        .path
        .split('/')
        .find(|segment| !segment.is_empty())
        .ok_or_else(|| {
            ConfigError::Value(format!(
                "{}: BaseURL path has no segments to derive asset_id from",
                path_str(mpd_path)
            ))
        })?;
    require_safe_path_component(first, "BaseURL (derived asset_id)", mpd_path).map(str::to_string)
}

/// `load_asset(mpd_path, key, output_root, asset_id=None)`: the asset id (the override, or
/// derived from the `BaseURL`), the download root, `output_root/<asset_id>` and the
/// renditions — in field insertion order, so the first failure is Python's first failure.
pub fn load_asset(
    mpd_path: &Path,
    key: KeyMaterial,
    output_root: &Path,
    asset_id: Option<&str>,
) -> Result<Asset, ConfigError> {
    let mpd_path = PurePosixPath::from_path(mpd_path).to_path_buf();
    let asset_id = match asset_id {
        Some(asset_id) if !asset_id.is_empty() => asset_id.to_string(),
        _ => derive_asset_id(&mpd_path)?,
    };
    let input_base_dir = find_download_root(&mpd_path)?;
    let output_dir = PurePosixPath::from_path(output_root)
        .join(&asset_id)
        .to_path_buf();
    let renditions = enumerate_renditions(&mpd_path)?;
    Ok(Asset {
        asset_id,
        mpd_path,
        input_base_dir,
        output_dir,
        key,
        renditions,
    })
}
