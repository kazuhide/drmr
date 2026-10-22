//! Operator roots and role-bound, identity-checked references.
use super::wire::{Error, Result, Role, invalid, unavailable};
use crate::{ServerConfig, http::Req, pyutil};
use drmr_core::native::{self, FileIdentity};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    fs::{File, OpenOptions},
    io::Read,
    path::{Component, Path, PathBuf},
};

const MAX_REFS: usize = 16_384;
#[derive(Clone)]
pub struct BoundPath {
    pub path: PathBuf,
    identities: Vec<(PathBuf, FileIdentity)>,
}
impl BoundPath {
    pub fn capture(path: &Path) -> Result<Self> {
        if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(unavailable());
        }
        let mut current = PathBuf::new();
        let mut identities = Vec::new();
        for part in path.components() {
            current.push(part);
            // A Windows drive prefix becomes an absolute directory at RootDir.
            if matches!(part, Component::Prefix(_)) {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&current).map_err(|_| unavailable())?;
            if unsafe_link(&metadata) {
                return Err(unavailable());
            }
            let file = if metadata.is_dir() {
                open_directory(&current)
            } else if metadata.is_file() {
                open_regular(&current)
            } else {
                return Err(unavailable());
            }
            .map_err(|_| unavailable())?;
            identities.push((
                current.clone(),
                FileIdentity::read(&file).map_err(|_| unavailable())?,
            ));
        }
        if identities.is_empty() {
            return Err(unavailable());
        }
        Ok(Self {
            path: path.to_path_buf(),
            identities,
        })
    }
    pub fn check(&self) -> Result<()> {
        let now = Self::capture(&self.path)?;
        if now.identities == self.identities {
            Ok(())
        } else {
            Err(unavailable())
        }
    }
}
fn unsafe_link(m: &std::fs::Metadata) -> bool {
    if m.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if m.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}
fn open_directory(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options
            .custom_flags(0x0200_0000 | 0x0020_0000)
            .share_mode(7);
    }
    options.open(path)
}
fn open_regular(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000).share_mode(1);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    Ok(file)
}
#[derive(Clone)]
struct Root {
    id: String,
    label: &'static str,
    roles: Vec<Role>,
    binding: BoundPath,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    OutputParent,
}
#[derive(Clone)]
struct Reference {
    root: String,
    role: Role,
    kind: Kind,
    binding: BoundPath,
}
struct Cursor {
    root: String,
    role: Role,
    directory: PathBuf,
    last: OsString,
    binding: BoundPath,
}
pub struct Files {
    retained_base: PathBuf,
    roots: Vec<Root>,
    refs: HashMap<String, Reference>,
    cursors: HashMap<String, Cursor>,
    jobs: PathBuf,
    config: Option<PathBuf>,
}
impl std::fmt::Debug for Files {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommonFiles")
            .field("roots", &self.roots.len())
            .finish_non_exhaustive()
    }
}
impl Files {
    pub fn new(config: &ServerConfig) -> std::io::Result<Self> {
        let jobs = native::resolve_path(&config.output_root.join("_jobs"))?;
        let secret = config
            .common_config_root
            .as_deref()
            .map(std::fs::canonicalize)
            .transpose()?;
        if let Some(private) = &secret {
            for public in [&config.input_root, &config.output_root]
                .into_iter()
                .chain(config.static_dir.iter())
            {
                let public = native::resolve_path(public)?;
                if private.starts_with(&public) || public.starts_with(private) {
                    return Err(std::io::Error::other(
                        "private configuration root overlaps an HTTP root",
                    ));
                }
            }
            if !private.is_dir() || BoundPath::capture(private).is_err() {
                return Err(std::io::Error::other(
                    "private configuration root unavailable",
                ));
            }
        }
        let mut this = Self {
            retained_base: native::resolve_path(&config.output_root.join("_common"))?,
            roots: Vec::new(),
            refs: HashMap::new(),
            cursors: HashMap::new(),
            jobs,
            config: secret,
        };
        this.add_root(
            &config.input_root,
            "Input folder",
            vec![Role::Media, Role::SavedPlan],
        );
        this.add_root(
            &config.output_root,
            "Output folder",
            vec![Role::Media, Role::SavedPlan, Role::OutputParent],
        );
        if let Some(path) = this.config.clone() {
            this.add_root(
                &path,
                "Private configuration folder",
                vec![Role::Keys, Role::FetchPolicy, Role::SavedPlan],
            );
        }
        Ok(this)
    }
    fn add_root(&mut self, path: &Path, label: &'static str, roles: Vec<Role>) {
        if let Ok(path) = path.canonicalize()
            && path.is_dir()
            && let Ok(binding) = BoundPath::capture(&path)
        {
            self.roots.push(Root {
                id: pyutil::uuid4_hex(),
                label,
                roles,
                binding,
            });
        }
    }
    pub fn roots(&self) -> Value {
        json!(
            self.roots
                .iter()
                .map(|r| json!({"root_ref":r.id,"label":r.label,"roles":r.roles}))
                .collect::<Vec<_>>()
        )
    }
    pub fn http_denied(&self, path: &Path) -> bool {
        self.denied(path)
            || native::resolve_path(path).map_or(true, |p| p.starts_with(&self.retained_base))
    }
    pub fn denied(&self, path: &Path) -> bool {
        let Ok(path) = native::resolve_path(path) else {
            return true;
        };
        path.starts_with(&self.jobs)
            || self
                .config
                .as_ref()
                .is_some_and(|root| path.starts_with(root))
            || path.components().any(|c| c.as_os_str() == "_jobs")
    }
    fn private_config_selection(&self, root: &Root, role: Role, path: &Path) -> bool {
        matches!(role, Role::Keys | Role::FetchPolicy | Role::SavedPlan)
            && root.roles.contains(&role)
            && self
                .config
                .as_ref()
                .is_some_and(|private| root.binding.path == *private && path.starts_with(private))
    }
    fn selection_denied(&self, root: &Root, role: Role, path: &Path) -> bool {
        let Ok(path) = native::resolve_path(path) else {
            return true;
        };
        path.starts_with(&self.jobs)
            || path.starts_with(&self.retained_base)
            || path.components().any(|c| c.as_os_str() == "_jobs")
            || self
                .config
                .as_ref()
                .is_some_and(|private| path.starts_with(private))
                && !self.private_config_selection(root, role, &path)
    }
    pub fn allowed_media(&self, path: &Path) -> Result<BoundPath> {
        if self.denied(path) {
            return Err(unavailable());
        }
        let binding = BoundPath::capture(path)?;
        if self.roots.iter().any(|r| {
            r.roles.contains(&Role::Media)
                && path.starts_with(&r.binding.path)
                && r.binding.check().is_ok()
        }) {
            Ok(binding)
        } else {
            Err(unavailable())
        }
    }
    pub fn allowed_output(&self, path: &Path) -> Result<()> {
        if self.denied(path) {
            return Err(unavailable());
        }
        let parent = path.parent().ok_or_else(unavailable)?;
        BoundPath::capture(parent)?.check()?;
        if self.roots.iter().any(|r| {
            r.roles.contains(&Role::OutputParent)
                && parent.starts_with(&r.binding.path)
                && r.binding.check().is_ok()
        }) {
            Ok(())
        } else {
            Err(unavailable())
        }
    }
    pub fn file(&self, id: &str, role: Role) -> Result<BoundPath> {
        self.reference(id, role, Kind::File)
    }
    pub fn output_parent(&self, id: &str) -> Result<BoundPath> {
        self.reference(id, Role::OutputParent, Kind::OutputParent)
    }
    fn reference(&self, id: &str, role: Role, kind: Kind) -> Result<BoundPath> {
        let r = self.refs.get(id).ok_or_else(unavailable)?;
        if r.role != role || r.kind != kind {
            return Err(unavailable());
        }
        let root = self
            .roots
            .iter()
            .find(|x| x.id == r.root)
            .ok_or_else(unavailable)?;
        root.binding.check()?;
        r.binding.check()?;
        if !r.binding.path.starts_with(&root.binding.path)
            || self.selection_denied(root, role, &r.binding.path)
        {
            return Err(unavailable());
        }
        Ok(r.binding.clone())
    }
    fn issue(&mut self, root: &Root, role: Role, kind: Kind, binding: BoundPath) -> Result<String> {
        if let Some((id, _)) = self.refs.iter().find(|(_, r)| {
            r.root == root.id
                && r.role == role
                && r.kind == kind
                && r.binding.path == binding.path
                && r.binding.identities == binding.identities
        }) {
            return Ok(id.clone());
        }
        if self.refs.len() + self.cursors.len() >= MAX_REFS {
            return Err(Error::new(409, "reference-limit-reached"));
        }
        let id = pyutil::uuid4_hex();
        self.refs.insert(
            id.clone(),
            Reference {
                root: root.id.clone(),
                role,
                kind,
                binding,
            },
        );
        Ok(id)
    }
    pub fn issue_media(&mut self, path: &Path) -> Result<String> {
        let binding = self.allowed_media(path)?;
        let root = self
            .roots
            .iter()
            .find(|r| r.roles.contains(&Role::Media) && path.starts_with(&r.binding.path))
            .cloned()
            .ok_or_else(unavailable)?;
        self.issue(&root, Role::Media, Kind::File, binding)
    }
    pub fn page(&mut self, req: &Req) -> Result<Value> {
        let mut query = BTreeMap::new();
        for (k, v) in &req.args {
            if !matches!(
                k.as_str(),
                "root_ref" | "role" | "directory_ref" | "cursor" | "limit"
            ) || query.insert(k.as_str(), v.as_str()).is_some()
            {
                return Err(invalid());
            }
        }
        let root_id = *query.get("root_ref").ok_or_else(invalid)?;
        let role: Role = serde_json::from_value(json!(query.get("role").ok_or_else(invalid)?))
            .map_err(|_| invalid())?;
        let root = self
            .roots
            .iter()
            .find(|r| r.id == root_id && r.roles.contains(&role))
            .cloned()
            .ok_or_else(unavailable)?;
        root.binding.check()?;
        let directory_ref = query.get("directory_ref").copied();
        let directory = match directory_ref {
            Some(id) => {
                if self
                    .refs
                    .get(id)
                    .is_none_or(|reference| reference.root != root.id)
                {
                    return Err(unavailable());
                }
                self.reference(id, role, Kind::Directory)?
            }
            None => root.binding.clone(),
        };
        if !directory.path.starts_with(&root.binding.path) {
            return Err(unavailable());
        }
        let last = match query.get("cursor") {
            None => None,
            Some(id) => {
                let c = self.cursors.get(*id).ok_or_else(unavailable)?;
                c.binding.check()?;
                if c.root != root.id || c.role != role || c.directory != directory.path {
                    return Err(unavailable());
                }
                Some(c.last.clone())
            }
        };
        let limit = match query.get("limit") {
            None => 100,
            Some(n) => n
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=100).contains(n))
                .ok_or_else(invalid)?,
        };
        let selected = self.scan(&root, &directory, role, last.as_ref(), limit)?;
        directory.check()?;
        let more = selected.len() > limit;
        let (entries, last_name) = self.entries(&root, role, selected, limit)?;
        let cursor = if more {
            if self.refs.len() + self.cursors.len() >= MAX_REFS {
                return Err(Error::new(409, "reference-limit-reached"));
            }
            let id = pyutil::uuid4_hex();
            self.cursors.insert(
                id.clone(),
                Cursor {
                    root: root.id.clone(),
                    role,
                    directory: directory.path.clone(),
                    last: last_name.ok_or_else(unavailable)?,
                    binding: directory.clone(),
                },
            );
            Some(id)
        } else {
            None
        };
        let parent = if role == Role::OutputParent {
            Some(self.issue(&root, role, Kind::OutputParent, directory)?)
        } else {
            None
        };
        Ok(
            json!({"root_ref":root.id,"role":role,"directory_ref":directory_ref,"output_parent_ref":parent,"entries":entries,"next_cursor":cursor}),
        )
    }
    fn scan(
        &self,
        root: &Root,
        directory: &BoundPath,
        role: Role,
        last: Option<&OsString>,
        limit: usize,
    ) -> Result<BTreeMap<OsString, PathBuf>> {
        let mut selected = BTreeMap::new();
        for e in std::fs::read_dir(&directory.path)
            .map_err(|_| unavailable())?
            .flatten()
        {
            let name = e.file_name();
            if last.is_some_and(|x| &name <= x) {
                continue;
            }
            let path = e.path();
            if name.to_string_lossy().starts_with('.') || self.selection_denied(root, role, &path) {
                continue;
            }
            let Ok(meta) = e.metadata() else {
                continue;
            };
            if unsafe_link(&std::fs::symlink_metadata(&path).map_err(|_| unavailable())?)
                || !meta.is_dir() && !eligible(&path, role)
            {
                continue;
            }
            selected.insert(name, path);
            if selected.len() > limit + 1 {
                selected.pop_last();
            }
        }
        Ok(selected)
    }
    fn entries(
        &mut self,
        root: &Root,
        role: Role,
        selected: BTreeMap<OsString, PathBuf>,
        limit: usize,
    ) -> Result<(Vec<Value>, Option<OsString>)> {
        let mut entries = Vec::new();
        let mut last_name = None;
        for (i, (name, path)) in selected.into_iter().take(limit).enumerate() {
            let Ok(binding) = BoundPath::capture(&path) else {
                continue;
            };
            let is_dir = path.is_dir();
            let id = self.issue(
                root,
                role,
                if is_dir { Kind::Directory } else { Kind::File },
                binding,
            )?;
            let label = if self.private_config_selection(root, role, &path) {
                format!(
                    "{} {}",
                    if is_dir { "Folder" } else { "Configuration" },
                    i + 1
                )
            } else {
                safe_label(&name.to_string_lossy())
            };
            entries.push(if is_dir {
                json!({"kind":"directory","label":label,"directory_ref":id})
            } else {
                json!({"kind":"file","label":label,"file_ref":id})
            });
            last_name = Some(name);
        }
        Ok((entries, last_name))
    }
}
fn eligible(path: &Path, role: Role) -> bool {
    if !path.is_file() {
        return false;
    }
    let extension = path
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match role {
        Role::Media => matches!(extension.as_str(), "mp4" | "m4a" | "mpd" | "m3u8"),
        Role::SavedPlan | Role::Keys | Role::FetchPolicy => extension == "json",
        Role::OutputParent => false,
    }
}
fn safe_label(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(128).collect()
}
pub fn read_private(binding: &BoundPath, limit: usize) -> Result<Vec<u8>> {
    binding.check()?;
    let file = native::open_private_read(&binding.path)
        .map_err(|_| Error::new(422, "private-config-unavailable"))?;
    if file.metadata().map_err(|_| unavailable())?.len() > u64::try_from(limit).unwrap_or(u64::MAX)
    {
        return Err(Error::new(413, "request-too-large"));
    }
    let id = FileIdentity::read(&file).map_err(|_| unavailable())?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(limit).unwrap_or(u64::MAX) + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.len() > limit {
        return Err(Error::new(413, "request-too-large"));
    }
    binding.check()?;
    let reread = native::open_private_read(&binding.path).map_err(|_| unavailable())?;
    if FileIdentity::read(&reread).map_err(|_| unavailable())? != id {
        return Err(unavailable());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::BoundPath;

    #[test]
    fn absolute_roots_and_children_bind_every_filesystem_component() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let volume = root.ancestors().last().unwrap();
        BoundPath::capture(volume).unwrap().check().unwrap();
        BoundPath::capture(&root).unwrap().check().unwrap();
        let parent = root.join("media");
        std::fs::create_dir(&parent).unwrap();
        let media = parent.join("source.mp4");
        std::fs::write(&media, b"original").unwrap();
        let child = BoundPath::capture(&media).unwrap();
        child.check().unwrap();

        std::fs::rename(&media, parent.join("original.mp4")).unwrap();
        std::fs::write(&media, b"original").unwrap();
        assert!(child.check().is_err());

        let replacement = BoundPath::capture(&media).unwrap();
        std::fs::rename(&parent, root.join("original-directory")).unwrap();
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(&media, b"original").unwrap();
        assert!(replacement.check().is_err());
    }

    #[cfg(windows)]
    #[test]
    fn ordinary_and_extended_windows_drive_paths_are_both_admitted() {
        let temporary = tempfile::tempdir().unwrap();
        let extended = temporary.path().canonicalize().unwrap();
        let text = extended.to_str().unwrap();
        let ordinary = text.strip_prefix(r"\\?\").unwrap();
        assert!(std::path::Path::new(ordinary).is_absolute());
        BoundPath::capture(std::path::Path::new(ordinary))
            .unwrap()
            .check()
            .unwrap();
        BoundPath::capture(&extended).unwrap().check().unwrap();
        let file = extended.join("source.mp4");
        std::fs::write(&file, b"original").unwrap();
        BoundPath::capture(&file).unwrap().check().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn prefix_only_windows_roots_cannot_create_empty_identity_bindings() {
        for text in [r"\\?\C:", r"\\?\UNC\example.invalid\share"] {
            let path = std::path::Path::new(text);
            assert!(path.is_absolute());
            assert_eq!(path.components().count(), 1);
            assert!(BoundPath::capture(path).is_err());
        }
    }
}
