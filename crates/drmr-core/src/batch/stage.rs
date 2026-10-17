//! Discovery, confinement, and staging of an asset's source graph.
//!
//! The graph must be disjoint from the output prefix before any deletion occurs.
//! Staged resources preserve the download layout based on host and URL path.

use super::{
    AssetWorkItem, AttemptError, BatchConfig, BatchError, HlsSource, RunError, TRANSIENT_SOURCE,
    classify, commands, is_output_key, private_config, run_commands,
};
use crate::batch_support::{
    as_uri, is_relative_to, py_splitlines, py_strip, resolve, unquote, urljoin, urlsplit,
};
use crate::m3u8::parse_attributes;
use crate::storage::{ObjectRef, StorageError};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The DASH namespace `ET.findall("m:…")` resolves `m` to.
const MPD_NS: &str = "urn:mpeg:dash:schema:mpd:2011";
/// A manifest or playlist larger than this is `malformed-manifest`.
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
/// At most this many playlists per HLS graph.
const MAX_PLAYLISTS: usize = 512;

fn malformed() -> AttemptError {
    BatchError::new("malformed-manifest").into()
}

fn value_error(_: crate::batch_support::UrlError) -> AttemptError {
    AttemptError::Value("invalid-url".to_string())
}

/// `_source_ref`: an absolute `http(s)` URL without user information or fragment as the
/// `netloc/path` object key (the query is not part of the key).
pub fn source_ref(url: &str) -> Result<ObjectRef, AttemptError> {
    let parsed = urlsplit(url, "").map_err(value_error)?;
    if !matches!(parsed.scheme.as_str(), "https" | "http")
        || parsed.netloc.is_empty()
        || parsed.username().is_some_and(|name| !name.is_empty())
        || parsed
            .password()
            .is_some_and(|password| !password.is_empty())
        || !parsed.fragment.is_empty()
    {
        return Err(malformed());
    }
    Ok(ObjectRef::new(format!(
        "{}/{}",
        parsed.netloc,
        parsed.path.trim_start_matches('/')
    ))?)
}

/// `_manifest_bytes`: at most 16 MiB.
fn manifest_bytes(path: &Path) -> Result<Vec<u8>, AttemptError> {
    let metadata = std::fs::metadata(path).map_err(|_| AttemptError::Other)?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(malformed());
    }
    std::fs::read(path).map_err(|_| AttemptError::Other)
}

/// `_source_overlaps_output`: whether a source URL lands in the owned output — a local file
/// under the output root or the store's output prefix, an S3 URL of the configured bucket under
/// the prefix (virtual-hosted or path style), or a mirror key under the prefix. An access
/// point or object-lambda host cannot be attributed (`hls-source-ownership-unqualified`).
pub fn source_overlaps_output(url: &str, config: &BatchConfig) -> Result<bool, AttemptError> {
    let parsed = urlsplit(url, "").map_err(value_error)?;
    let prefix = config.output_prefix.as_str();
    if parsed.scheme == "file" {
        let local = resolve(Path::new(&unquote(&parsed.path)));
        if is_relative_to(&local, &resolve(&config.output_root)) {
            return Ok(true);
        }
        return Ok(config
            .storage
            .root()
            .is_some_and(|root| is_relative_to(&local, &root.join(prefix))));
    }
    if !matches!(parsed.scheme.as_str(), "https" | "http") {
        return Ok(false);
    }
    let host = parsed.hostname().unwrap_or_default().to_lowercase();
    let mut object_key = None;
    if let Some(bucket) = config.storage.bucket().filter(|bucket| !bucket.is_empty()) {
        let path = unquote(&parsed.path);
        let path = path.trim_start_matches('/');
        if host.starts_with(&format!("{}.s3", bucket.to_lowercase()))
            && host.ends_with(".amazonaws.com")
        {
            object_key = Some(path.to_string());
        } else if (host == "s3.amazonaws.com" || host.starts_with("s3.") || host.starts_with("s3-"))
            && host.ends_with(".amazonaws.com")
        {
            object_key = path.strip_prefix(&format!("{bucket}/")).map(str::to_string);
        }
        if host.contains(".s3-accesspoint.") || host.contains(".s3-object-lambda.") {
            return Err(BatchError::unsafe_source_graph("hls-source-ownership-unqualified").into());
        }
    }
    if let Some(key) = object_key {
        return Ok(is_output_key(&key, prefix));
    }
    Ok(is_output_key(source_ref(url)?.key(), prefix))
}

/// The MPD facts `_stage_inputs` and the preflight read with `ElementTree`.
struct Mpd {
    /// `(contentType, mimeType, [codecs of its Representation children])` per `AdaptationSet`.
    adaptation_sets: Vec<(Option<String>, String, Vec<String>)>,
    /// The root's first `BaseURL` child's text (`None`: no such child or no text).
    base_url: Option<String>,
    /// `(initialization, media)` of every `SegmentTemplate`.
    templates: Vec<(Option<String>, Option<String>)>,
}

/// Parse an MPD as `ET.fromstring` does (after the caller's `<!DOCTYPE` refusal); a parse
/// error is `ET.ParseError` (`malformed-manifest`).
fn parse_mpd(raw: &[u8]) -> Result<Mpd, AttemptError> {
    let text = std::str::from_utf8(raw).map_err(|_| AttemptError::Value("parse-error".into()))?;
    let document =
        roxmltree::Document::parse(text).map_err(|_| AttemptError::Value("parse-error".into()))?;
    let root = document.root_element();
    let is = |node: &roxmltree::Node<'_, '_>, name: &str| {
        node.is_element()
            && node.tag_name().namespace() == Some(MPD_NS)
            && node.tag_name().name() == name
    };
    let mut mpd = Mpd {
        adaptation_sets: Vec::new(),
        base_url: None,
        templates: Vec::new(),
    };
    for node in root.descendants().skip(1) {
        if is(&node, "AdaptationSet") {
            let codecs = node
                .children()
                .filter(|child| is(child, "Representation"))
                .map(|child| child.attribute("codecs").unwrap_or_default().to_string())
                .collect();
            mpd.adaptation_sets.push((
                node.attribute("contentType").map(str::to_string),
                node.attribute("mimeType").unwrap_or_default().to_string(),
                codecs,
            ));
        } else if is(&node, "SegmentTemplate") {
            mpd.templates.push((
                node.attribute("initialization").map(str::to_string),
                node.attribute("media").map(str::to_string),
            ));
        }
    }
    mpd.base_url = root
        .children()
        .find(|child| is(child, "BaseURL"))
        .and_then(element_text);
    Ok(mpd)
}

/// `ElementTree`'s `element.text`: the character data before the first child element (comments
/// skipped), `None` when there is none.
fn element_text(node: roxmltree::Node<'_, '_>) -> Option<String> {
    let mut text = String::new();
    let mut any = false;
    for child in node.children() {
        if child.is_element() {
            break;
        }
        if let Some(data) = child.text() {
            any = true;
            text.push_str(data);
        }
    }
    (any && !text.is_empty()).then_some(text)
}

/// `urljoin(base, value)` as an object key.
fn joined_ref(base: &str, value: &str) -> Result<ObjectRef, AttemptError> {
    source_ref(&urljoin(base, value).map_err(value_error)?)
}

/// The staging state of `_stage_inputs`: what is already downloaded and the bytes staged.
struct Stager<'a> {
    config: &'a BatchConfig,
    stage: &'a Path,
    downloaded: HashSet<String>,
    staged_bytes: u64,
}

impl Stager<'_> {
    /// `get(refs)`: the objects not yet downloaded, after the output-prefix, object-count and
    /// byte budgets (`TransferPolicy` of the configuration).
    fn get(&mut self, refs: &[ObjectRef]) -> Result<(), AttemptError> {
        let mut keys = HashSet::new();
        let unique: Vec<ObjectRef> = refs
            .iter()
            .filter(|object| {
                !self.downloaded.contains(object.key()) && keys.insert(object.key().to_string())
            })
            .cloned()
            .collect();
        let prefix = &self.config.output_prefix;
        if unique
            .iter()
            .any(|object| is_output_key(object.key(), prefix))
        {
            return Err(malformed());
        }
        let budget = usize::try_from(self.config.transfer.max_objects).unwrap_or(usize::MAX);
        if self.downloaded.len() + unique.len() > budget {
            return Err(StorageError::new("storage-object-budget").into());
        }
        let cap = u64::try_from(self.config.transfer.max_asset_bytes).unwrap_or(u64::MAX);
        let mut size: u64 = 0;
        for object in &unique {
            size = size.saturating_add(self.config.storage.object_size(object)?);
        }
        if self.staged_bytes.saturating_add(size) > cap {
            return Err(StorageError::new("storage-byte-budget").into());
        }
        self.config.storage.get_many(&unique, self.stage)?;
        for object in &unique {
            let actual = std::fs::metadata(self.stage.join(object.key()))
                .map_err(|_| AttemptError::Other)?
                .len();
            self.staged_bytes = self.staged_bytes.saturating_add(actual);
        }
        if self.staged_bytes > cap {
            return Err(StorageError::new("storage-byte-budget").into());
        }
        self.downloaded.extend(keys);
        Ok(())
    }
}

/// `_stage_inputs`: the asset's source graph staged under
/// `output/<asset_id>/clear/_batch-stage/download/` (`dash-cenc`), or the clear-HLS analysis
/// run and verified (`hls-clear`). The graph is validated before the caller deletes anything.
pub fn stage_inputs(
    work: &AssetWorkItem,
    config: &BatchConfig,
    source: &dyn HlsSource,
) -> Result<PathBuf, AttemptError> {
    let stage = config
        .output_root
        .join(&work.asset_id)
        .join("clear/_batch-stage/download");
    std::fs::create_dir_all(&stage).map_err(|_| AttemptError::Other)?;
    if work.kind == "hls-clear" {
        stage_clear(work, config, &stage, source)?;
        return Ok(stage);
    }
    let Some(manifest) = work
        .dash_manifest
        .as_ref()
        .filter(|_| work.kind == "dash-cenc")
    else {
        return Err(BatchError::new("unsupported-kind").into());
    };
    let mut stager = Stager {
        config,
        stage: &stage,
        downloaded: HashSet::new(),
        staged_bytes: 0,
    };
    let mut refs = vec![manifest.clone()];
    refs.extend(work.hls_master.clone());
    if refs
        .iter()
        .any(|object| is_output_key(object.key(), &config.output_prefix))
    {
        return Err(malformed());
    }
    stager.get(&refs)?;
    let raw = manifest_bytes(&stage.join(manifest.key()))?;
    if contains(&raw, b"<!DOCTYPE") {
        return Err(malformed());
    }
    let mpd = parse_mpd(&raw)?;
    check_scope(&mpd)?;
    stage_media(&mpd, &mut stager)?;
    if let Some(master) = &work.hls_master {
        stage_playlists(master, &mut stager)?;
    }
    Ok(stage)
}

/// Subtitles and out-of-scope codecs are refused before any staging of media.
fn check_scope(mpd: &Mpd) -> Result<(), AttemptError> {
    for (content_type, mime, codecs) in &mpd.adaptation_sets {
        if matches!(content_type.as_deref(), Some("text" | "application"))
            || mime.starts_with("text/")
            || mime.starts_with("application/")
        {
            return Err(BatchError::new("unsupported-subtitles").into());
        }
        let supported = ["avc1", "hvc1", "mp4a.40", "vp9", "vp09"];
        if codecs
            .iter()
            .any(|codec| !supported.iter().any(|known| codec.starts_with(known)))
        {
            return Err(BatchError::new("unsupported-codec").into());
        }
    }
    Ok(())
}

/// Everything under the `BaseURL` directory, every initialization segment and every media
/// directory a `SegmentTemplate` names.
fn stage_media(mpd: &Mpd, stager: &mut Stager<'_>) -> Result<(), AttemptError> {
    let base = mpd.base_url.as_deref().ok_or_else(malformed)?;
    let base = py_strip(base);
    let placeholder = joined_ref(base, "placeholder")?;
    let prefix = parent_key(placeholder.key());
    if is_output_key(prefix, &stager.config.output_prefix) {
        return Err(malformed());
    }
    let media = stager
        .config
        .storage
        .list_prefix(&ObjectRef::new(prefix)?)?;
    stager.get(&media)?;
    for (initialization, media) in &mpd.templates {
        for (attribute, value) in [("initialization", initialization), ("media", media)] {
            let Some(value) = value.as_deref().filter(|value| !value.is_empty()) else {
                continue;
            };
            let object = joined_ref(base, value)?;
            if is_output_key(object.key(), &stager.config.output_prefix) {
                return Err(malformed());
            }
            if attribute == "initialization" {
                stager.get(&[object])?;
            } else {
                let directory = ObjectRef::new(parent_key(object.key()))?;
                let listed = stager.config.storage.list_prefix(&directory)?;
                stager.get(&listed)?;
            }
        }
    }
    Ok(())
}

/// `key.rsplit("/", 1)[0]`.
fn parent_key(key: &str) -> &str {
    key.rsplit_once('/').map_or(key, |(parent, _)| parent)
}

/// The HLS graph from the master: every referenced object staged (keys, session keys and
/// session data excepted), nested `.m3u8` playlists followed (at most 512).
fn stage_playlists(master: &ObjectRef, stager: &mut Stager<'_>) -> Result<(), AttemptError> {
    let mut pending = vec![master.clone()];
    let mut seen = HashSet::new();
    let mut count = 0;
    while let Some(object) = pending.pop() {
        if !seen.insert(object.key().to_string()) {
            continue;
        }
        count += 1;
        if count > MAX_PLAYLISTS {
            return Err(malformed());
        }
        let raw = manifest_bytes(&stager.stage.join(object.key()))?;
        let text = decode_utf8_sig(&raw)?;
        if !text.starts_with("#EXTM3U") {
            return Err(malformed());
        }
        if text.contains("SUBTITLES=") || text.contains("TYPE=SUBTITLES") {
            return Err(BatchError::new("unsupported-subtitles").into());
        }
        let mut children = Vec::new();
        for line in py_splitlines(text) {
            let line = py_strip(line);
            let Some(uri) = line_uri(line)? else {
                continue;
            };
            let scheme = urlsplit(&uri, "").map_err(value_error)?.scheme;
            if !matches!(scheme.as_str(), "" | "http" | "https") {
                continue;
            }
            let child = joined_ref(&format!("https://{}", object.key()), &uri)?;
            if is_output_key(child.key(), &stager.config.output_prefix) {
                return Err(malformed());
            }
            let excluded = ["#EXT-X-KEY:", "#EXT-X-SESSION-KEY:", "#EXT-X-SESSION-DATA:"];
            if !excluded.iter().any(|tag| line.starts_with(tag)) {
                children.push(child);
            }
        }
        stager.get(&children)?;
        pending.extend(
            children
                .into_iter()
                .filter(|child| child.key().to_lowercase().ends_with(".m3u8")),
        );
    }
    Ok(())
}

/// A playlist line's URI: the line itself when it is not a tag, the `URI` attribute of a tag
/// that has one (`None` otherwise).
fn line_uri(line: &str) -> Result<Option<String>, AttemptError> {
    if !line.is_empty() && !line.starts_with('#') {
        return Ok(Some(line.to_string()));
    }
    if line.starts_with('#') && line.contains("URI=") {
        let list = line.split_once(':').map_or("", |(_, rest)| rest);
        let attributes =
            parse_attributes(list).map_err(|error| AttemptError::Value(error.token().into()))?;
        return Ok(attributes
            .get("URI")
            .filter(|uri| !uri.is_empty())
            .map(str::to_string));
    }
    Ok(None)
}

/// `bytes.decode("utf-8-sig")`.
fn decode_utf8_sig(raw: &[u8]) -> Result<&str, AttemptError> {
    let raw = raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(raw);
    std::str::from_utf8(raw).map_err(|_| AttemptError::Value("unicode-decode-error".into()))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// The clear-HLS half of `_stage_inputs`: run `analyze-hls` when no verified snapshot exists,
/// verify the snapshot again, and refuse any recorded resource inside the owned output.
fn stage_clear(
    work: &AssetWorkItem,
    config: &BatchConfig,
    stage: &Path,
    source: &dyn HlsSource,
) -> Result<(), AttemptError> {
    let planned = commands(work, config, stage, source)?;
    let analyze: Vec<_> = planned
        .into_iter()
        .filter(|command| command.subcommand == "analyze-hls")
        .collect();
    if !analyze.is_empty() {
        let code = run_commands(&analyze, config, None, &|| {})?;
        if code != 0 {
            let token = if code == 1 {
                "content-fail"
            } else {
                "hls-config-invalid"
            };
            return Err(BatchError::new(token).into());
        }
    }
    // Both fresh and reused analyses are verified before any owned-prefix delete.
    commands(work, config, stage, source)?;
    let analysis = config
        .output_root
        .join(&work.asset_id)
        .join("clear/source/analysis.json");
    let raw = std::fs::read(&analysis).map_err(|_| AttemptError::Other)?;
    let private: Value = serde_json::from_slice(&raw)
        .map_err(|_| AttemptError::Value("json-decode-error".into()))?;
    let resources = private
        .get("resources")
        .and_then(Value::as_array)
        .ok_or(AttemptError::Other)?;
    for resource in resources {
        let url = resource
            .get("url")
            .and_then(Value::as_str)
            .ok_or(AttemptError::Other)?;
        if source_overlaps_output(url, config)? {
            return Err(BatchError::new("hls-config-invalid").into());
        }
    }
    Ok(())
}

/// `_preflight_source_graphs`: every work item's source graph is read (manifests only; media
/// references are inspected as locations) and proven disjoint from the owned output before any
/// asset may delete anything. An unsafe graph aborts the whole run; an unreadable one too —
/// retryable when the store was unavailable, otherwise `source-preflight-unavailable` (a
/// transient source failure) or `source-preflight-unproven`.
pub fn preflight_source_graphs(
    work_list: &[AssetWorkItem],
    config: &BatchConfig,
    source: &dyn HlsSource,
) -> Result<(), RunError> {
    let temporary_root = config.output_root.join("_batch");
    std::fs::create_dir_all(&temporary_root)?;
    for work in work_list {
        if work.kind != "dash-cenc" && work.kind != "hls-clear" {
            continue;
        }
        let result = tempfile::Builder::new()
            .prefix("preflight-")
            .tempdir_in(&temporary_root)
            .map_err(|_| AttemptError::Other)
            .and_then(|directory| {
                if work.kind == "hls-clear" {
                    preflight_clear(work, config, directory.path(), source)
                } else {
                    preflight_dash(work, config, directory.path())
                }
            });
        match result {
            Ok(()) => {}
            Err(AttemptError::Batch(error)) if error.is_unsafe_source_graph() => {
                return Err(error.into());
            }
            Err(AttemptError::Interrupted(message)) => return Err(RunError::Interrupted(message)),
            Err(error) => {
                if classify(&error).1 {
                    return Err(StorageError::retryable("storage-transient").into());
                }
                let token = if TRANSIENT_SOURCE.contains(&error.to_string().as_str()) {
                    "source-preflight-unavailable"
                } else {
                    "source-preflight-unproven"
                };
                return Err(BatchError::new(token).into());
            }
        }
    }
    Ok(())
}

fn unsafe_overlap() -> AttemptError {
    BatchError::unsafe_source_graph("source-output-overlap").into()
}

/// One `dash-cenc` graph: the MPD's `BaseURL` and `SegmentTemplate` locations and every
/// playlist of the HLS graph, read one manifest at a time into `directory`.
fn preflight_dash(
    work: &AssetWorkItem,
    config: &BatchConfig,
    directory: &Path,
) -> Result<(), AttemptError> {
    let prefix = config.output_prefix.as_str();
    let check = |object: &ObjectRef| {
        if is_output_key(object.key(), prefix) {
            Err(unsafe_overlap())
        } else {
            Ok(())
        }
    };
    let read = |object: &ObjectRef| -> Result<Vec<u8>, AttemptError> {
        check(object)?;
        if config.storage.object_size(object)? > MAX_MANIFEST_BYTES {
            return Err(malformed());
        }
        config
            .storage
            .get_many(std::slice::from_ref(object), directory)?;
        manifest_bytes(&directory.join(object.key()))
    };
    let manifest = work.dash_manifest.as_ref().ok_or_else(malformed)?;
    let raw = read(manifest)?;
    if contains(&raw, b"<!DOCTYPE") {
        return Err(malformed());
    }
    let mpd = parse_mpd(&raw)?;
    let base = mpd.base_url.as_deref().ok_or_else(malformed)?;
    let base = py_strip(base);
    check(&joined_ref(base, "placeholder")?)?;
    for (initialization, media) in &mpd.templates {
        for value in [initialization, media].into_iter().flatten() {
            if !value.is_empty() {
                check(&joined_ref(base, value)?)?;
            }
        }
    }
    let mut pending: Vec<ObjectRef> = work.hls_master.iter().cloned().collect();
    let mut seen = HashSet::new();
    while let Some(object) = pending.pop() {
        if !seen.insert(object.key().to_string()) {
            continue;
        }
        if seen.len() > MAX_PLAYLISTS {
            return Err(malformed());
        }
        let raw = read(&object)?;
        let lines = py_splitlines(decode_utf8_sig(&raw)?);
        if lines
            .first()
            .is_none_or(|first| py_strip(first) != "#EXTM3U")
        {
            return Err(malformed());
        }
        let is_master = lines
            .iter()
            .any(|line| line.starts_with("#EXT-X-STREAM-INF:"));
        for line in lines {
            let line = py_strip(line);
            let (uri, playlist) = if !line.is_empty() && !line.starts_with('#') {
                (Some(line.to_string()), is_master)
            } else {
                let playlist = line.starts_with("#EXT-X-MEDIA:")
                    || line.starts_with("#EXT-X-I-FRAME-STREAM-INF:");
                (line_uri(line)?, playlist)
            };
            let Some(uri) = uri else {
                continue;
            };
            let scheme = urlsplit(&uri, "").map_err(value_error)?.scheme;
            if matches!(scheme.as_str(), "" | "https" | "http") {
                let child = joined_ref(&format!("https://{}", object.key()), &uri)?;
                check(&child)?;
                if playlist {
                    pending.push(child);
                }
            }
        }
    }
    Ok(())
}

/// One `hls-clear` graph: the private source configuration's playlists, fetched under its
/// fetch policy, with every playlist, effective URL and child location checked.
fn preflight_clear(
    work: &AssetWorkItem,
    config: &BatchConfig,
    directory: &Path,
    source: &dyn HlsSource,
) -> Result<(), AttemptError> {
    let profiles = config
        .hls_config_dir
        .as_deref()
        .ok_or_else(|| BatchError::new("hls-config-invalid"))?;
    let profiles = resolve(profiles);
    let settings = private_config(
        &profiles.join(format!("{}.json", work.asset_id)),
        &config.output_root,
    )?;
    let source_name = settings
        .get("source_config")
        .and_then(Value::as_str)
        .ok_or(AttemptError::Other)?;
    let source_config = private_config(&resolve(&profiles.join(source_name)), &config.output_root)?;
    let mut location = source_config
        .get("url")
        .and_then(Value::as_str)
        .ok_or(AttemptError::Other)?
        .to_string();
    if urlsplit(&location, "")
        .map_err(value_error)?
        .scheme
        .is_empty()
    {
        location = as_uri(&resolve(Path::new(&location)));
    }
    let empty = Value::Object(serde_json::Map::new());
    let policy = source_config.get("policy").unwrap_or(&empty);
    let mut fetcher = source.open_fetcher(policy, directory)?;
    let result = walk_clear(&location, config, fetcher.as_mut(), source);
    fetcher.close_all();
    result
}

fn walk_clear(
    location: &str,
    config: &BatchConfig,
    fetcher: &mut dyn super::PlaylistFetcher,
    source: &dyn HlsSource,
) -> Result<(), AttemptError> {
    let mut pending = vec![location.to_string()];
    let mut seen = HashSet::new();
    while let Some(location) = pending.pop() {
        if !seen.insert(location.clone()) {
            continue;
        }
        if source_overlaps_output(&location, config)? {
            return Err(unsafe_overlap());
        }
        let (body, effective) = fetcher.read_playlist(&location)?;
        if source_overlaps_output(&effective, config)? {
            return Err(unsafe_overlap());
        }
        let lines = source.lines(&body)?;
        let master = lines
            .iter()
            .any(|line| line.starts_with("#EXT-X-STREAM-INF:"));
        for line in &lines {
            let (uri, playlist) = if line.starts_with('#') {
                if !line.contains("URI=") {
                    continue;
                }
                (line_uri(line)?, line.starts_with("#EXT-X-MEDIA:"))
            } else {
                (Some(line.clone()), master)
            };
            let Some(uri) = uri else {
                continue;
            };
            let child = source.child_url(&effective, &uri)?;
            if source_overlaps_output(&child, config)? {
                return Err(unsafe_overlap());
            }
            if playlist {
                pending.push(child);
            }
        }
    }
    Ok(())
}
