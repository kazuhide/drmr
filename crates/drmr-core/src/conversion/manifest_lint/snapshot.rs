use super::{CHANGED, CONFIG, Converter, GRAPH, ManifestLink};
use crate::conversion::{ConversionError, files};
use crate::native::FileIdentity;
use drmr_manifest::{DocumentId, DocumentSize, Format, Limits, Options};
use sha2::{Digest as _, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    io::Read as _,
    path::{Component, Path, PathBuf},
};

const INVALID: ConversionError = ConversionError("manifest-unreadable");
const BUFFER: usize = 64 * 1024;

pub(super) struct Node {
    pub id: DocumentId,
    pub bytes: usize,
    pub base_uri: String,
    relative: PathBuf,
    identity: FileIdentity,
    digest: [u8; 32],
    lines: usize,
}

/// Only small manifest metadata survives discovery; raw graphs are loaded after admission.
pub(super) struct Snapshot {
    root: PathBuf,
    format: Format,
    pub nodes: Vec<Node>,
}

/// Discovery retains its lease through the caller's scalar sizing/sealing work.
/// Field order also keeps metadata charged if the caller returns or unwinds.
pub(super) struct MetadataSnapshot {
    pub snapshot: Snapshot,
    _permit: crate::resources::Permit,
}

impl MetadataSnapshot {
    /// Bundle callers already charged this retained metadata before discovery.
    /// Release the discovery leaf before any separately admitted guard check.
    pub fn into_snapshot(self) -> Snapshot {
        self.snapshot
    }
}

impl Snapshot {
    pub fn capture_metadata(
        converter: &Converter,
        entrypoint: &Path,
        format: Format,
    ) -> Result<MetadataSnapshot, ConversionError> {
        // This is the same finite graph walker and working-set contract used
        // by FrozenSource capture. It discovers only manifests here; resource
        // callbacks do not read media, key addresses or remote references.
        let bytes =
            usize::try_from(crate::conversion::frozen::DISCOVERY_BYTES).map_err(|_| GRAPH)?;
        let permit = converter.manifest_permit(1, bytes)?;
        if std::fs::symlink_metadata(entrypoint)
            .map_err(|_| INVALID)?
            .file_type()
            .is_symlink()
        {
            return Err(INVALID);
        }
        let entrypoint = entrypoint.canonicalize().map_err(|_| INVALID)?;
        let root = entrypoint.parent().ok_or(INVALID)?.to_path_buf();
        if root.as_os_str().len() > 32768 {
            return Err(GRAPH);
        }
        let first = entrypoint
            .strip_prefix(&root)
            .map_err(|_| INVALID)?
            .to_path_buf();
        let mut result = Self {
            root: root.clone(),
            format,
            nodes: Vec::new(),
        };
        let mut seen = BTreeSet::new();
        let mut total = 0_usize;
        crate::conversion::frozen::walk_manifests(
            &root,
            &first,
            &converter.backend.stop,
            |relative| {
                if !seen.insert(relative.to_path_buf()) {
                    return Ok(());
                }
                let node = result.discover_admitted(converter, relative)?;
                total = total
                    .checked_add(node.bytes)
                    .filter(|bytes| *bytes <= 64 * 1024 * 1024)
                    .ok_or(GRAPH)?;
                result.nodes.push(node);
                Ok(())
            },
        )?;
        result.check_admitted(converter)?;
        Ok(MetadataSnapshot {
            snapshot: result,
            _permit: permit,
        })
    }

    pub fn seal(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"drmr-manifest-sizing-v1\0");
        hash.update([u8::from(self.format != Format::Hls)]);
        let nodes: BTreeMap<_, _> = self
            .nodes
            .iter()
            .map(|node| (&node.relative, node))
            .collect();
        for (path, node) in nodes {
            hash.update((path.as_os_str().len() as u64).to_be_bytes());
            hash.update(path.as_os_str().as_encoded_bytes());
            hash.update((node.bytes as u64).to_be_bytes());
            hash.update(node.digest);
        }
        hash.finalize().into()
    }

    pub fn capture(
        converter: &Converter,
        entrypoint: &Path,
        format: Format,
        options: &Options,
    ) -> Result<Self, ConversionError> {
        if std::fs::symlink_metadata(entrypoint)
            .map_err(|_| INVALID)?
            .file_type()
            .is_symlink()
        {
            return Err(INVALID);
        }
        let entrypoint = entrypoint.canonicalize().map_err(|_| INVALID)?;
        let root = entrypoint.parent().ok_or(INVALID)?.to_path_buf();
        let first = entrypoint
            .strip_prefix(&root)
            .map_err(|_| INVALID)?
            .to_path_buf();
        let mut result = Self {
            root,
            format,
            nodes: Vec::new(),
        };
        let mut pending = VecDeque::from([first.clone()]);
        let mut queued = BTreeSet::from([first]);
        let mut total = 0_usize;
        while let Some(relative) = pending.pop_front() {
            converter.backend.check_cancelled()?;
            let node = match result.discover(converter, &relative) {
                Ok(node) => node,
                Err(error) if result.nodes.is_empty() || fatal(error) => return Err(error),
                // Missing or unsafe child playlists remain unresolved in native graph coverage.
                Err(_) => continue,
            };
            total = total
                .checked_add(node.bytes)
                .filter(|total| *total <= options.limits.max_total_bytes)
                .ok_or(GRAPH)?;
            if format == Format::Hls {
                let mut one = options.clone();
                one.limits.memory_budget_bytes = one
                    .limits
                    .memory_budget_bytes
                    .checked_sub(result.metadata_bytes())
                    .filter(|&bytes| bytes > 0)
                    .ok_or(ConversionError("resource-budget"))?;
                one.workers = std::num::NonZeroUsize::new(1);
                one.limits.max_references = node.lines.max(1);
                let estimate = drmr_manifest::estimate_document_sizes(
                    &[DocumentSize {
                        id: node.id,
                        format,
                        byte_count: node.bytes,
                    }],
                    &one,
                )
                .map_err(|_| CONFIG)?;
                if estimate.admitted_workers == 0 {
                    return Err(ConversionError("resource-budget"));
                }
                let _permit = converter.manifest_permit(
                    1,
                    estimate
                        .estimated_peak_bytes
                        .saturating_add(result.metadata_bytes()),
                )?;
                let bytes = result.read_node(converter, &node)?;
                if let Ok(references) =
                    drmr_manifest::child_manifest_references(&bytes, &one.limits)
                {
                    for reference in references {
                        let Some(child) = resolve(&relative, reference.uri) else {
                            continue;
                        };
                        if queued.insert(child.clone()) {
                            if queued.len() > options.limits.max_documents {
                                return Err(GRAPH);
                            }
                            pending.push_back(child);
                        }
                    }
                }
            }
            result.nodes.push(node);
        }
        Ok(result)
    }

    fn discover(&self, converter: &Converter, relative: &Path) -> Result<Node, ConversionError> {
        let _permit =
            converter.manifest_permit(1, self.metadata_bytes().saturating_add(BUFFER * 2))?;
        self.discover_admitted(converter, relative)
    }

    fn discover_admitted(
        &self,
        converter: &Converter,
        relative: &Path,
    ) -> Result<Node, ConversionError> {
        self.path(relative)?;
        let path = self.root.join(relative);
        let mut file = files::regular_input(&path).map_err(|_| INVALID)?;
        let length = file.metadata().map_err(|_| INVALID)?.len();
        if length > crate::inspection::MAX_MANIFEST_BYTES {
            return Err(ConversionError("manifest-size-limit"));
        }
        let bytes = usize::try_from(length).map_err(|_| GRAPH)?;
        let identity = FileIdentity::read(&file).map_err(|_| INVALID)?;
        let (digest, lines) = hash(converter, &mut file, bytes)?;
        Ok(Node {
            id: DocumentId(u32::try_from(self.nodes.len()).map_err(|_| GRAPH)?),
            bytes,
            relative: relative.to_path_buf(),
            base_uri: document_base(relative),
            identity,
            digest,
            lines,
        })
    }

    pub fn reference_bound(&self) -> usize {
        if self.format == Format::Dash {
            1
        } else {
            self.nodes
                .iter()
                .map(|node| node.lines)
                .sum::<usize>()
                .max(1)
        }
    }

    pub fn metadata_bytes(&self) -> usize {
        // The same bounded root charge applies before and after private copying;
        // relative manifest paths and content, rather than temporary root names,
        // determine a bundle's stable sizing promise.
        (4 * 32768_usize)
            .saturating_add(std::mem::size_of::<Self>() * 4)
            .saturating_add(
                self.nodes
                    .iter()
                    .map(|node| {
                        node.relative
                            .as_os_str()
                            .len()
                            .saturating_mul(4)
                            .saturating_add(node.base_uri.len())
                            .saturating_add(std::mem::size_of::<Node>() * 4)
                    })
                    .sum::<usize>(),
            )
    }

    fn path(&self, relative: &Path) -> Result<(), ConversionError> {
        let mut path = self.root.clone();
        for component in relative.components() {
            path.push(component);
            let metadata = std::fs::symlink_metadata(&path).map_err(|_| INVALID)?;
            if metadata.file_type().is_symlink() {
                return Err(INVALID);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt as _;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(INVALID);
                }
            }
        }
        Ok(())
    }

    fn read_node(&self, converter: &Converter, node: &Node) -> Result<Vec<u8>, ConversionError> {
        self.path(&node.relative).map_err(|_| CHANGED)?;
        let mut file =
            files::regular_input(&self.root.join(&node.relative)).map_err(|_| CHANGED)?;
        if FileIdentity::read(&file).map_err(|_| CHANGED)? != node.identity {
            return Err(CHANGED);
        }
        let mut bytes = Vec::with_capacity(node.bytes);
        let mut buffer = vec![0; BUFFER].into_boxed_slice();
        loop {
            converter.backend.check_cancelled()?;
            let count = file.read(&mut buffer).map_err(|_| CHANGED)?;
            if count == 0 {
                break;
            }
            if bytes
                .len()
                .checked_add(count)
                .is_none_or(|length| length > node.bytes)
            {
                return Err(CHANGED);
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        if bytes.len() != node.bytes || <[u8; 32]>::from(Sha256::digest(&bytes)) != node.digest {
            return Err(CHANGED);
        }
        Ok(bytes)
    }

    pub fn read(&self, converter: &Converter) -> Result<Vec<Vec<u8>>, ConversionError> {
        self.nodes
            .iter()
            .map(|node| self.read_node(converter, node))
            .collect()
    }

    pub fn links<'a>(&self, bytes: &'a [Vec<u8>], limits: &Limits) -> Vec<ManifestLink<'a>> {
        if self.format == Format::Dash {
            return Vec::new();
        }
        let ids: BTreeMap<_, _> = self
            .nodes
            .iter()
            .map(|node| (&node.relative, node.id))
            .collect();
        let mut links = Vec::new();
        let mut unique = BTreeSet::new();
        for (node, bytes) in self.nodes.iter().zip(bytes) {
            if let Ok(references) = drmr_manifest::child_manifest_references(bytes, limits) {
                for reference in references {
                    let Some(relative) = resolve(&node.relative, reference.uri) else {
                        continue;
                    };
                    if let Some(&to) = ids.get(&relative)
                        && unique.insert((node.id, reference.uri))
                    {
                        links.push(ManifestLink {
                            from: node.id,
                            reference: reference.uri,
                            to,
                        });
                    }
                }
            }
        }
        links
    }

    pub fn check(&self, converter: &Converter) -> Result<(), ConversionError> {
        let _permit =
            converter.manifest_permit(1, self.metadata_bytes().saturating_add(BUFFER * 2))?;
        self.check_admitted(converter)
    }

    pub fn check_admitted(&self, converter: &Converter) -> Result<(), ConversionError> {
        for node in &self.nodes {
            converter.backend.check_cancelled()?;
            self.path(&node.relative).map_err(|_| CHANGED)?;
            let mut file =
                files::regular_input(&self.root.join(&node.relative)).map_err(|_| CHANGED)?;
            if FileIdentity::read(&file).map_err(|_| CHANGED)? != node.identity {
                return Err(CHANGED);
            }
            if hash(converter, &mut file, node.bytes)?.0 != node.digest {
                return Err(CHANGED);
            }
        }
        converter.backend.check_cancelled()
    }
}

fn hash(
    converter: &Converter,
    file: &mut std::fs::File,
    expected: usize,
) -> Result<([u8; 32], usize), ConversionError> {
    let mut hash = Sha256::new();
    let mut bytes = 0_usize;
    let mut lines = 1_usize;
    let mut buffer = vec![0; BUFFER].into_boxed_slice();
    loop {
        converter.backend.check_cancelled()?;
        let count = file.read(&mut buffer).map_err(|_| CHANGED)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count)
            .filter(|bytes| *bytes <= expected)
            .ok_or(CHANGED)?;
        lines += memchr::memchr_iter(b'\n', &buffer[..count]).count();
        hash.update(&buffer[..count]);
    }
    if bytes != expected {
        return Err(CHANGED);
    }
    Ok((hash.finalize().into(), lines))
}

fn fatal(error: ConversionError) -> bool {
    matches!(
        error.0,
        "cancelled"
            | "resource-budget"
            | "manifest-changed-during-validation"
            | "manifest-size-limit"
            | "manifest-validation-graph-limit"
    )
}

fn resolve(parent: &Path, reference: &str) -> Option<PathBuf> {
    if reference.is_empty()
        || reference.len() > 4096
        || reference.starts_with('/')
        || reference.contains([':', '?', '#', '%', '\\'])
        || reference.chars().any(char::is_control)
    {
        return None;
    }
    let mut result = parent.parent().unwrap_or(Path::new("")).to_path_buf();
    for component in Path::new(reference).components() {
        match component {
            Component::Normal(value) => result.push(value),
            Component::CurDir => {}
            Component::ParentDir if result.pop() => {}
            _ => return None,
        }
    }
    (!result.as_os_str().is_empty()).then_some(result)
}

fn document_base(relative: &Path) -> String {
    use std::fmt::Write as _;
    let mut uri = String::from("https://manifest.invalid/");
    let mut first = true;
    for component in relative.components() {
        let Component::Normal(value) = component else {
            continue;
        };
        if !first {
            uri.push('/');
        }
        first = false;
        for &byte in value.as_encoded_bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                uri.push(char::from(byte));
            } else {
                write!(uri, "%{byte:02X}").expect("formatting a String cannot fail");
            }
        }
    }
    uri
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn document_bases_preserve_components_and_encode_path_bytes_without_real_origins() {
        assert_eq!(
            document_base(Path::new("nested dir/a%/média.m3u8")),
            "https://manifest.invalid/nested%20dir/a%25/m%C3%A9dia.m3u8"
        );
        assert_eq!(
            document_base(Path::new("master.m3u8")),
            "https://manifest.invalid/master.m3u8"
        );
    }
}
