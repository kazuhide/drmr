//! Asset discovery and validation for the staging catalog.

use crate::pyutil::{self, ResolveError};
use drmr_core::config::{KeyMaterial, load_key_material};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// The `asset.json` fields every entry must carry (non-empty).
const REQUIRED_FIELDS: [&str; 5] = [
    "asset_id",
    "label",
    "mpd_path",
    "hls_master_path",
    "keys_file",
];

/// One staged, fully validated asset.
#[derive(Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    /// The id (`output/<asset_id>/`, the `/source` and `/output` URL space).
    pub asset_id: String,
    /// The display label.
    pub label: String,
    /// The resolved DASH manifest.
    pub mpd_path: PathBuf,
    /// The resolved HLS master playlist.
    pub hls_master_path: PathBuf,
    /// This asset's own KID, key and IV (from its `keys.env`).
    pub key_material: KeyMaterial,
    /// The resolved entry directory (its name is not necessarily the `asset_id`).
    pub entry_dir: PathBuf,
}

impl fmt::Debug for CatalogEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CatalogEntry")
            .field("asset_id", &self.asset_id)
            .field("label", &self.label)
            .field("mpd_path", &self.mpd_path)
            .field("hls_master_path", &self.hls_master_path)
            .field("key_material", &self.key_material)
            .field("entry_dir", &self.entry_dir)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogScanError {
    /// `catalog_root.iterdir()` failed.
    #[error("catalog root cannot be listed")]
    List(#[source] std::io::Error),
    /// `Path.resolve()` met a symlink loop (`RuntimeError`).
    #[error("symlink loop")]
    SymlinkLoop,
}

impl CatalogScanError {
    pub fn class_name(&self) -> &'static str {
        match self {
            CatalogScanError::List(error) => pyutil::exception_name(error),
            CatalogScanError::SymlinkLoop => "RuntimeError",
        }
    }
}

/// `_SAFE_ASSET_ID_RE.fullmatch(value)`: `[A-Za-z0-9_-][A-Za-z0-9._-]*`.
pub fn is_safe_asset_id(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric() || first == b'_' || first == b'-')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

enum EntryError {
    Skip(String),
    Escape(CatalogScanError),
}

impl From<String> for EntryError {
    fn from(reason: String) -> Self {
        EntryError::Skip(reason)
    }
}

fn resolve(path: &Path) -> Result<PathBuf, EntryError> {
    pyutil::resolve(path).map_err(|error| match error {
        // `os.lstat` raises `ValueError("embedded null byte")`: a skip whose reason is that text.
        ResolveError::Nul => EntryError::Skip("embedded null byte".to_string()),
        ResolveError::Loop => EntryError::Escape(CatalogScanError::SymlinkLoop),
    })
}

fn parse_env_file(path: &Path) -> Result<HashMap<String, String>, EntryError> {
    let text = std::fs::read(path)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(|| {
            EntryError::Skip(format!(
                "{}: keys file is unreadable or not valid UTF-8 text",
                pyutil::path_text(path)
            ))
        })?;
    let mut env = HashMap::new();
    for raw_line in py_splitlines(&text) {
        let line = drmr_core::hls_cli::py_strip(raw_line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        env.insert(
            drmr_core::hls_cli::py_strip(name).to_string(),
            drmr_core::hls_cli::py_strip(value).to_string(),
        );
    }
    Ok(env)
}

/// `str.splitlines()`: `\n`, `\r`, `\r\n`, `\v`, `\f`, `\x1c`–`\x1e`, `\x85`, U+2028, U+2029.
fn py_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            lines.push(&text[start..index]);
            let mut end = index + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
                chars.next();
                end += 1;
            }
            start = end;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// `_resolve_within(entry_dir, rel, field)`: a non-empty relative path from `asset.json`,
/// resolved and confined to the entry directory, naming an existing file.
fn resolve_within(entry_dir: &Path, rel: &Value, field: &str) -> Result<PathBuf, EntryError> {
    let text = match rel {
        Value::String(text) if !text.is_empty() => text,
        _ => {
            return Err(EntryError::Skip(format!(
                "{field}: path must be a non-empty string, got {}",
                pyutil::repr_value(rel)
            )));
        }
    };
    let entry_resolved = resolve(entry_dir)?;
    let candidate = resolve(&entry_resolved.join(text))?;
    if !pyutil::is_relative_to(&candidate, &entry_resolved) {
        return Err(EntryError::Skip(format!(
            "{field}: path escapes the entry directory: {}",
            pyutil::repr_str(text)
        )));
    }
    if !pyutil::is_file(&candidate) {
        return Err(EntryError::Skip(format!(
            "{field}: referenced file not found: {}",
            pyutil::repr_str(text)
        )));
    }
    Ok(candidate)
}

/// `_load_entry(entry_dir)`: a complete, valid entry or the first problem found.
fn load_entry(entry_dir: &Path) -> Result<CatalogEntry, EntryError> {
    let descriptor_path = entry_dir.join("asset.json");
    let shown = pyutil::path_text(&descriptor_path);
    if !pyutil::is_file(&descriptor_path) {
        return Err(EntryError::Skip(format!(
            "{}: no asset.json",
            pyutil::path_text(entry_dir)
        )));
    }
    let descriptor = read_descriptor(&descriptor_path)
        .map_err(|detail| format!("{shown}: unreadable/invalid JSON: {detail}"))?;
    let Value::Object(descriptor) = descriptor else {
        return Err(EntryError::Skip(format!(
            "{shown}: asset.json must be a JSON object"
        )));
    };
    let missing: Vec<&str> = REQUIRED_FIELDS
        .iter()
        .copied()
        .filter(|field| {
            !descriptor
                .get(*field)
                .is_some_and(drmr_core::hls_cli::truthy)
        })
        .collect();
    if !missing.is_empty() {
        return Err(EntryError::Skip(format!(
            "{shown}: missing required field(s): {}",
            missing.join(", ")
        )));
    }
    let asset_id = match &descriptor["asset_id"] {
        Value::String(text) if is_safe_asset_id(text) => text.clone(),
        other => {
            return Err(EntryError::Skip(format!(
                "{shown}: unsafe asset_id: {}",
                pyutil::repr_value(other)
            )));
        }
    };
    let Value::String(label) = &descriptor["label"] else {
        return Err(EntryError::Skip(format!("{shown}: label must be a string")));
    };
    let mpd_path = resolve_within(entry_dir, &descriptor["mpd_path"], "mpd_path")?;
    let hls_master_path =
        resolve_within(entry_dir, &descriptor["hls_master_path"], "hls_master_path")?;
    let keys_path = resolve_within(entry_dir, &descriptor["keys_file"], "keys_file")?;
    // `/source` serves the resolved download tree: the key file must never be reachable there.
    let download_root = resolve(&entry_dir.join("download"))?;
    if pyutil::is_relative_to(&keys_path, &download_root) {
        return Err(EntryError::Skip(
            "keys_file: must be outside the served download directory".to_string(),
        ));
    }
    let env = parse_env_file(&keys_path)?;
    // `config.load_key_material` raises `ValueError` only here (every value is text); its
    // message names the variable or the field and a byte count, never a value.
    let key_material = load_key_material(Some(&env)).map_err(|error| {
        format!(
            "{}: invalid key material: {error}",
            pyutil::path_text(&keys_path)
        )
    })?;
    Ok(CatalogEntry {
        asset_id,
        label: label.clone(),
        mpd_path,
        hls_master_path,
        key_material,
        entry_dir: resolve(entry_dir)?,
    })
}

fn read_descriptor(path: &Path) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|error| pyutil::oserror_text(&error, path))?;
    let text = String::from_utf8(bytes).map_err(|error| {
        let position = error.utf8_error().valid_up_to();
        let byte = error.as_bytes()[position];
        format!("'utf-8' codec can't decode byte 0x{byte:02x} in position {position}: invalid start byte")
    })?;
    serde_json::from_str::<Value>(&text).map_err(|error| {
        let line = error.line();
        let column = error.column();
        let offset: usize = text
            .split('\n')
            .take(line.saturating_sub(1))
            .map(|row| row.chars().count() + 1)
            .sum::<usize>()
            + column.saturating_sub(1);
        format!("Expecting value: line {line} column {column} (char {offset})")
    })
}

/// `on_skip(entry_dir, reason)`: told once per skipped directory.
pub type SkipHook<'a> = &'a mut dyn FnMut(&Path, &str);

pub fn discover_catalog(
    catalog_root: &Path,
    mut on_skip: Option<SkipHook<'_>>,
) -> Result<Vec<CatalogEntry>, CatalogScanError> {
    if !pyutil::is_dir(catalog_root) {
        return Ok(Vec::new());
    }
    let mut children: Vec<PathBuf> = std::fs::read_dir(catalog_root)
        .map_err(CatalogScanError::List)?
        .map(|entry| entry.map(|entry| catalog_root.join(entry.file_name())))
        .collect::<Result<_, _>>()
        .map_err(CatalogScanError::List)?;
    children.sort();
    let mut entries: Vec<CatalogEntry> = Vec::new();
    let mut first_dir: HashMap<String, PathBuf> = HashMap::new();
    for child in children {
        if !pyutil::is_dir(&child) {
            continue;
        }
        let outcome = load_entry(&child).and_then(|entry| match first_dir.get(&entry.asset_id) {
            Some(earlier) => Err(EntryError::Skip(format!(
                "{}: duplicate asset_id {} (already provided by {})",
                pyutil::path_text(&child),
                pyutil::repr_str(&entry.asset_id),
                pyutil::repr_str(
                    &earlier
                        .file_name()
                        .map_or(String::new(), |name| name.to_string_lossy().into_owned())
                ),
            ))),
            None => Ok(entry),
        });
        match outcome {
            Ok(entry) => {
                first_dir.insert(entry.asset_id.clone(), child);
                entries.push(entry);
            }
            Err(EntryError::Skip(reason)) => {
                if let Some(callback) = on_skip.as_mut() {
                    callback(&child, &reason);
                }
            }
            Err(EntryError::Escape(error)) => return Err(error),
        }
    }
    entries.sort_by(|left, right| left.asset_id.cmp(&right.asset_id));
    Ok(entries)
}

/// `get_catalog_entry(catalog_root, asset_id)`: the entry with this id, `None` for an unknown
/// or unsafe id (never used to build a path).
pub fn get_catalog_entry(
    catalog_root: &Path,
    asset_id: &str,
) -> Result<Option<CatalogEntry>, CatalogScanError> {
    if !is_safe_asset_id(asset_id) {
        return Ok(None);
    }
    Ok(discover_catalog(catalog_root, None)?
        .into_iter()
        .find(|entry| entry.asset_id == asset_id))
}

/// `catalog_entry_public_dict(entry, converted=…)`: exactly `asset_id`, `label`, `converted`,
/// `entry` (the directory name) and `paths` (`mpd`, `hls_master`, relative to the entry).
pub fn catalog_entry_public_dict(entry: &CatalogEntry, converted: bool) -> Value {
    let relative = |path: &Path| {
        path.strip_prefix(&entry.entry_dir)
            .map_or_else(|_| String::new(), pyutil::path_text)
    };
    let mut paths = Map::new();
    paths.insert("mpd".into(), Value::from(relative(&entry.mpd_path)));
    paths.insert(
        "hls_master".into(),
        Value::from(relative(&entry.hls_master_path)),
    );
    let mut map = Map::new();
    map.insert("asset_id".into(), Value::from(entry.asset_id.clone()));
    map.insert("label".into(), Value::from(entry.label.clone()));
    map.insert("converted".into(), Value::Bool(converted));
    map.insert(
        "entry".into(),
        Value::from(
            entry
                .entry_dir
                .file_name()
                .map_or(String::new(), |name| name.to_string_lossy().into_owned()),
        ),
    );
    map.insert("paths".into(), Value::Object(paths));
    Value::Object(map)
}
