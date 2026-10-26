//! Exclusive download ownership and nonrecursive, identity-checked cleanup.
use super::super::{ConversionError, files};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

const UNSAFE: ConversionError = ConversionError("download-destination-unsafe");
const CLEANUP: ConversionError = ConversionError("download-cleanup-unconfirmed");
const OUTPUT_EXISTS: ConversionError = ConversionError("output-exists-or-unsafe");

use crate::native::FileIdentity as Identity;
mod accounting;
use accounting::Accounting;
#[derive(Clone, Copy)]
struct Entry {
    identity: Option<Identity>,
    directory: bool,
}

pub(super) struct OwnedDownload {
    path: PathBuf,
    root: Option<File>,
    root_id: Identity,
    parent: File,
    parent_id: Identity,
    #[cfg(windows)]
    _ancestors: Vec<File>,
    entries: Mutex<BTreeMap<PathBuf, Entry>>,
    accounting: Mutex<Accounting>,
    retained: bool,
    attempted: bool,
    cleaned: bool,
}
impl OwnedDownload {
    pub(super) fn reserve(download: &Path, output: &Path) -> Result<Self, ConversionError> {
        Self::reserve_inner(download, Some(output))
    }
    pub(super) fn reserve_source(download: &Path) -> Result<Self, ConversionError> {
        Self::reserve_inner(download, None)
    }
    fn reserve_inner(download: &Path, output: Option<&Path>) -> Result<Self, ConversionError> {
        let path = destination(download)?;
        let accounting = Accounting::new(path.capacity())?;
        let output = output.map(destination).transpose()?;
        match std::fs::symlink_metadata(&path) {
            Ok(_) => return Err(ConversionError("download-directory-exists")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(UNSAFE),
        }
        let parent_path = path.parent().ok_or(UNSAFE)?;
        #[cfg(windows)]
        let ancestors = ancestors(parent_path)?;
        let parent = open_dir(parent_path)?;
        let parent_id = identity(&parent)?;
        let stage = files::Stage::new(Some(parent_path))?;
        // Allocate a separate exclusive strict child before exposing the destination name.
        let reservation = stage.private_root().join("reservation");
        crate::native::create_private_dir_new(&reservation).map_err(|_| UNSAFE)?;
        let root_id = identity(&open_dir(&reservation)?)?;
        files::publish_directory(&reservation, &path, None)
            .map_err(|_| ConversionError("download-directory-exists"))?;
        let root = open_dir(&path)?;
        if identity(&root)? != root_id {
            return Err(UNSAFE);
        }
        let mut owned = Self {
            path,
            root: Some(root),
            root_id,
            parent,
            parent_id,
            #[cfg(windows)]
            _ancestors: ancestors,
            entries: Mutex::new(BTreeMap::new()),
            accounting: Mutex::new(accounting),
            retained: false,
            attempted: false,
            cleaned: false,
        };
        if let Some(output) = output
            && let Err(error) = owned.check_output(&output)
        {
            owned.cleanup()?;
            return Err(error);
        }
        owned.check_identity()?;
        Ok(owned)
    }
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
    pub(super) fn register_file(
        &self,
        path: &Path,
        created: Identity,
    ) -> Result<(), ConversionError> {
        self.register(path, false, created)
    }
    pub(super) fn create_dir(&self, path: &Path) -> Result<(), ConversionError> {
        relative(path)?;
        self.prepay(path, true)?;
        self.check_identity()?;
        let stage = files::Stage::new(Some(&self.path))?;
        let pending = stage.private_root().join("directory");
        crate::native::create_private_dir_new(&pending).map_err(|_| UNSAFE)?;
        let created = identity(&open_dir(&pending)?)?;
        files::publish_directory(&pending, &self.path.join(path), None)?;
        self.register(path, true, created)
    }
    pub(super) fn create_parents(&self, file: &Path) -> Result<(), ConversionError> {
        relative(file)?;
        self.prepay_file(file)?;
        self.check_identity()?;
        let parents = file
            .ancestors()
            .skip(1)
            .filter(|path| !path.as_os_str().is_empty())
            .collect::<Vec<_>>();
        for parent in parents.into_iter().rev() {
            let known = self
                .entries
                .lock()
                .map_err(|_| UNSAFE)?
                .get(parent)
                .is_some_and(|entry| entry.identity.is_some());
            if known {
                self.open_registered_directory(parent)?;
            } else {
                self.create_dir(parent)?;
            }
        }
        Ok(())
    }
    pub(super) fn prepay_file(&self, path: &Path) -> Result<(), ConversionError> {
        self.prepay(path, false)
    }
    fn prepay(&self, path: &Path, directory: bool) -> Result<(), ConversionError> {
        relative(path)?;
        let mut entries = self.entries.lock().map_err(|_| UNSAFE)?;
        for ancestor in path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
            if entries
                .get(ancestor)
                .is_some_and(|entry| entry.directory != (ancestor != path || directory))
            {
                return Err(UNSAFE);
            }
        }
        let mut paths = path
            .ancestors()
            .filter(|p| !p.as_os_str().is_empty())
            .filter(|p| !entries.contains_key(*p))
            .map(|p| (p.to_path_buf(), p != path || directory))
            .collect::<Vec<_>>();
        self.accounting
            .lock()
            .map_err(|_| UNSAFE)?
            .prepay(&self.path, &paths)?;
        for (path, directory) in paths.drain(..) {
            entries.insert(
                path,
                Entry {
                    identity: None,
                    directory,
                },
            );
        }
        Ok(())
    }
    pub(super) fn charge_graph(&self, bytes: usize) -> Result<(), ConversionError> {
        self.accounting.lock().map_err(|_| UNSAFE)?.graph(bytes)
    }
    pub(super) fn check_expansion(&self, bytes: usize) -> Result<(), ConversionError> {
        self.accounting.lock().map_err(|_| UNSAFE)?.expansion(bytes)
    }
    pub(super) fn retained_bytes(&self) -> Result<u64, ConversionError> {
        self.accounting.lock().map_err(|_| UNSAFE)?.finish()
    }
    fn register(
        &self,
        path: &Path,
        directory: bool,
        created: Identity,
    ) -> Result<(), ConversionError> {
        relative(path)?;
        self.check_identity()?;
        let mut entries = self.entries.lock().map_err(|_| UNSAFE)?;
        self.check_parents(path, &entries)?;
        if entries
            .get(path)
            .is_none_or(|entry| entry.identity.is_some() || entry.directory != directory)
        {
            return Err(UNSAFE);
        }
        let file = if directory {
            open_dir(&self.path.join(path))?
        } else {
            crate::native::open_private_read(&self.path.join(path)).map_err(|_| UNSAFE)?
        };
        let current = identity(&file)?;
        if created != current {
            return Err(UNSAFE);
        }
        entries.insert(
            path.to_path_buf(),
            Entry {
                identity: Some(current),
                directory,
            },
        );
        Ok(())
    }
    pub(super) fn check_identity(&self) -> Result<(), ConversionError> {
        if self.cleaned
            || identity(&self.parent)? != self.parent_id
            || identity(&open_dir(self.path.parent().ok_or(UNSAFE)?)?)? != self.parent_id
            || identity(&open_dir(&self.path)?)? != self.root_id
        {
            return Err(UNSAFE);
        }
        Ok(())
    }
    pub(super) fn check_output(&self, output: &Path) -> Result<(), ConversionError> {
        self.check_identity()?;
        let output = destination(output)?;
        let metadata = match std::fs::symlink_metadata(&output) {
            Ok(info) => Some(info),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(UNSAFE),
        };
        let output_id = match &metadata {
            Some(info) if info.is_dir() => {
                Some(identity(&open_dir(&output).map_err(|_| OUTPUT_EXISTS)?)?)
            }
            Some(info) if !info.is_file() => return Err(OUTPUT_EXISTS),
            _ => None,
        };
        if contains(self.root_id, &output)?
            || output_id
                .map(|id| contains(id, &self.path))
                .transpose()?
                .unwrap_or(false)
        {
            return Err(ConversionError("download-output-overlap"));
        }
        if metadata.is_some() {
            return Err(OUTPUT_EXISTS);
        }
        Ok(())
    }
    pub(super) fn check_registered(&self) -> Result<(), ConversionError> {
        self.check_identity()?;
        let entries = self.entries.lock().map_err(|_| UNSAFE)?;
        for (path, entry) in entries.iter().filter(|(_, entry)| entry.identity.is_some()) {
            self.check_parents(path, &entries)?;
            let file = if entry.directory {
                open_dir(&self.path.join(path))?
            } else {
                crate::native::open_private_read(&self.path.join(path)).map_err(|_| UNSAFE)?
            };
            if Some(identity(&file)?) != entry.identity {
                return Err(UNSAFE);
            }
        }
        Ok(())
    }
    pub(super) fn registered_count(&self) -> Result<usize, ConversionError> {
        Ok(self
            .entries
            .lock()
            .map_err(|_| UNSAFE)?
            .values()
            .filter(|e| e.identity.is_some())
            .count())
    }
    /// Retain the registered directory while traversing its entries. Unknown directories
    /// are never adopted, including empty directories omitted from the file digest set.
    pub(super) fn open_registered_directory(&self, path: &Path) -> Result<File, ConversionError> {
        self.check_identity()?;
        if path.as_os_str().is_empty() {
            return open_dir(&self.path);
        }
        relative(path)?;
        let entries = self.entries.lock().map_err(|_| UNSAFE)?;
        self.check_parents(path, &entries)?;
        let expected = entries
            .get(path)
            .filter(|entry| entry.directory && entry.identity.is_some())
            .ok_or(UNSAFE)?;
        let directory = open_dir(&self.path.join(path))?;
        if Some(identity(&directory)?) != expected.identity {
            return Err(UNSAFE);
        }
        Ok(directory)
    }
    fn check_parents(
        &self,
        path: &Path,
        entries: &BTreeMap<PathBuf, Entry>,
    ) -> Result<(), ConversionError> {
        for parent in path
            .ancestors()
            .skip(1)
            .filter(|path| !path.as_os_str().is_empty())
        {
            let entry = entries
                .get(parent)
                .filter(|entry| entry.directory && entry.identity.is_some())
                .ok_or(UNSAFE)?;
            if Some(identity(&open_dir(&self.path.join(parent))?)?) != entry.identity {
                return Err(UNSAFE);
            }
        }
        Ok(())
    }
    /// Call only after every writer and fetch worker has stopped and joined.
    pub(super) fn cleanup(&mut self) -> Result<(), ConversionError> {
        if self.cleaned || self.retained {
            return Ok(());
        }
        if self.attempted {
            return Err(CLEANUP);
        }
        self.attempted = true;
        self.check_identity().map_err(|_| CLEANUP)?;
        let entries = self.entries.lock().map_err(|_| CLEANUP)?;
        let mut order: Vec<_> = entries
            .iter()
            .filter(|(_, entry)| entry.identity.is_some())
            .collect();
        order.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
        let mut complete = true;
        for (relative, entry) in order {
            if self.check_identity().is_err() {
                complete = false;
                break;
            }
            let path = self.path.join(relative);
            let current = if entry.directory {
                open_dir(&path)
            } else {
                crate::native::open_private_read(&path).map_err(|_| UNSAFE)
            };
            let matches = self.check_parents(relative, &entries).is_ok()
                && current.as_ref().ok().and_then(|file| identity(file).ok()) == entry.identity;
            drop(current);
            if !matches {
                complete = false;
                continue;
            }
            let result = if entry.directory {
                std::fs::remove_dir(path)
            } else {
                std::fs::remove_file(path)
            };
            complete &= result.is_ok();
        }
        drop(entries);
        // Release only the root's deletion-denying handle, retaining its ancestors.
        if self.check_identity().is_ok() {
            self.root.take();
            complete &= std::fs::remove_dir(&self.path).is_ok();
        } else {
            complete = false;
        }
        self.cleaned = complete;
        if complete { Ok(()) } else { Err(CLEANUP) }
    }
    pub(super) fn retain(&mut self) {
        self.retained = true;
    }
}
impl Drop for OwnedDownload {
    fn drop(&mut self) {
        if !self.retained && !self.attempted {
            let _ = self.cleanup();
        }
    }
}

fn relative(path: &Path) -> Result<(), ConversionError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(UNSAFE);
    }
    #[cfg(windows)]
    {
        // Inventory traversal joins native paths. Treat native separators at
        // this internal boundary while retaining the logical spelling refusals.
        if path
            .to_str()
            .ok_or(UNSAFE)?
            .split(['/', '\\'])
            .any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(UNSAFE);
        }
        for component in path.components() {
            let Component::Normal(name) = component else {
                return Err(UNSAFE);
            };
            crate::native::validate_windows_relative_path(name.to_str().ok_or(UNSAFE)?)
                .map_err(|_| UNSAFE)?;
        }
    }
    Ok(())
}
fn destination(path: &Path) -> Result<PathBuf, ConversionError> {
    let leaf = path.file_name().ok_or(UNSAFE)?;
    relative(Path::new(leaf))?;
    let original = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Canonicalize the original parent: symlink/.. is an operating-system operation.
    let parent = std::fs::canonicalize(original).map_err(|_| UNSAFE)?;
    open_dir(&parent)?;
    let path = parent.join(leaf);
    #[cfg(windows)]
    files::check_destination(&path)?;
    Ok(path)
}
fn contains(directory: Identity, path: &Path) -> Result<bool, ConversionError> {
    for (depth, ancestor) in path.ancestors().enumerate() {
        if depth >= 256 {
            return Err(UNSAFE);
        }
        match std::fs::symlink_metadata(ancestor) {
            Ok(info) if info.is_dir() => {
                if identity(&open_dir(ancestor)?)? == directory {
                    return Ok(true);
                }
            }
            Ok(info) if info.is_file() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) | Err(_) => return Err(UNSAFE),
        }
    }
    Ok(false)
}
fn open_dir(path: &Path) -> Result<File, ConversionError> {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.read(true).custom_flags(
            (rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options
            .access_mode(0x81)
            .share_mode(3)
            .custom_flags(0x0220_0000)
            .security_qos_flags(0x0001_0000);
    }
    let file = options.open(path).map_err(|_| UNSAFE)?;
    let metadata = file.metadata().map_err(|_| UNSAFE)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(UNSAFE);
        }
    }
    if !metadata.is_dir() {
        return Err(UNSAFE);
    }
    Ok(file)
}
fn identity(file: &File) -> Result<Identity, ConversionError> {
    Identity::read(file).map_err(|_| UNSAFE)
}
#[cfg(windows)]
fn ancestors(path: &Path) -> Result<Vec<File>, ConversionError> {
    let mut prefix = PathBuf::new();
    let mut held = Vec::new();
    for part in path.components() {
        prefix.push(part);
        if !matches!(part, Component::Prefix(_)) {
            held.push(open_dir(&prefix)?);
        }
        if held.len() > 256 {
            return Err(UNSAFE);
        }
    }
    Ok(held)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn a_prepaid_directory_cannot_be_reused_as_a_cheaper_file_reservation() {
        let parent = crate::native::private_tempdir().unwrap();
        let owned = OwnedDownload::reserve_source(&parent.path().join("source")).unwrap();
        owned.prepay(Path::new("video"), true).unwrap();
        assert_eq!(owned.prepay_file(Path::new("video")).unwrap_err(), UNSAFE);
        assert!(!owned.path().join("video").exists());
    }

    #[test]
    fn metadata_quota_precedes_every_parent_and_leaf_creation() {
        let parent = crate::native::private_tempdir().unwrap();
        let owned = OwnedDownload::reserve_source(&parent.path().join("source")).unwrap();
        let baseline = usize::try_from(owned.retained_bytes().unwrap()).unwrap();
        owned
            .charge_graph(super::super::GRAPH_BYTES - baseline)
            .unwrap();
        assert_eq!(
            owned
                .create_parents(Path::new("video/deep/segment.ts"))
                .unwrap_err(),
            ConversionError("download-graph-limit")
        );
        assert_eq!(std::fs::read_dir(owned.path()).unwrap().count(), 0);
        assert!(owned.entries.lock().unwrap().is_empty());
        assert_eq!(
            super::super::write_registered(&owned, Path::new("input.m3u8"), b"original", |_| {})
                .unwrap_err(),
            ConversionError("download-graph-limit")
        );
        assert!(!owned.path().join("input.m3u8").exists());
    }

    #[test]
    fn prepaid_creation_identity_is_registered_without_a_second_quota_request() {
        let parent = crate::native::private_tempdir().unwrap();
        let owned = OwnedDownload::reserve_source(&parent.path().join("source")).unwrap();
        let name = Path::new("original.mp4");
        owned.prepay_file(name).unwrap();
        let file = crate::native::create_private_new(&owned.path().join(name)).unwrap();
        let created = identity(&file).unwrap();
        drop(file);
        owned.accounting.lock().unwrap().maximum = 0;
        owned.register_file(name, created).unwrap();
        owned.check_registered().unwrap();
    }

    #[test]
    fn entrypoint_publication_is_prepaid_before_moving_the_pending_original() {
        let parent = crate::native::private_tempdir().unwrap();
        let owned = OwnedDownload::reserve_source(&parent.path().join("source")).unwrap();
        let stage = files::Stage::new(Some(parent.path())).unwrap();
        let pending = stage.path().join("entrypoint");
        let mut writer = crate::native::create_media_new(&pending).unwrap();
        writer.write_all(b"unchanged pending original").unwrap();
        let created = identity(&writer).unwrap();
        drop(writer);
        owned.accounting.lock().unwrap().maximum = 0;
        assert_eq!(
            super::super::publish_entrypoint(
                &owned,
                Path::new("input.mp4"),
                &pending,
                created,
                |_| {}
            )
            .unwrap_err(),
            ConversionError("download-graph-limit")
        );
        assert!(!owned.path().join("input.mp4").exists());
        assert_eq!(
            std::fs::read(pending).unwrap(),
            b"unchanged pending original"
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_joined_paths_preserve_logical_component_confinement() {
        for path in [
            PathBuf::from("video/1080p/segment.ts"),
            Path::new("video").join("1080p").join("segment.ts"),
        ] {
            relative(&path).unwrap();
        }
        for path in [
            "video/./1080p",
            "video//1080p",
            "video/1080p/",
            r"video\\1080p",
            "video/../outside",
            r"C:\outside",
            r"\outside",
            "video/CON",
            "video/con.txt",
            "video/COM1",
            "video/part:secret",
            "video/trailing.",
            "video/a?b",
        ] {
            assert!(
                relative(Path::new(path)).is_err(),
                "unsafe internal path accepted"
            );
        }
        assert!(crate::native::validate_windows_relative_path(r"video\1080p").is_err());
    }

    fn file(owned: &OwnedDownload, relative: &str, bytes: &[u8]) {
        owned.prepay_file(Path::new(relative)).unwrap();
        let mut writer = crate::native::create_private_new(&owned.path().join(relative)).unwrap();
        let created = identity(&writer).unwrap();
        writer.write_all(bytes).unwrap();
        drop(writer);
        owned.register_file(Path::new(relative), created).unwrap();
    }

    #[test]
    fn directory_replacement_before_registration_is_not_adopted() {
        let root = tempfile::tempdir().unwrap();
        let mut owned =
            OwnedDownload::reserve(&root.path().join("download"), &root.path().join("output"))
                .unwrap();
        let directory = owned.path().join("raw");
        crate::native::create_private_dir_new(&directory).unwrap();
        let created = identity(&open_dir(&directory).unwrap()).unwrap();
        std::fs::rename(&directory, root.path().join("original")).unwrap();
        crate::native::create_private_dir_new(&directory).unwrap();
        crate::native::create_private_new(&directory.join("sentinel"))
            .unwrap()
            .write_all(b"foreign directory")
            .unwrap();
        assert!(owned.register(Path::new("raw"), true, created).is_err());
        assert_eq!(owned.cleanup().unwrap_err(), CLEANUP);
        assert_eq!(
            std::fs::read(directory.join("sentinel")).unwrap(),
            b"foreign directory"
        );
    }

    #[test]
    fn replacement_before_registration_is_never_adopted_or_deleted() {
        for name in ["media-resource", "raw-playlist", "projection", "input.m3u8"] {
            let root = tempfile::tempdir().unwrap();
            let destination = root.path().join("download");
            let mut owned =
                OwnedDownload::reserve(&destination, &root.path().join("output")).unwrap();
            let path = destination.join(name);
            let mut writer = crate::native::create_private_new(&path).unwrap();
            writer.write_all(b"downloaded original").unwrap();
            let created = identity(&writer).unwrap();
            drop(writer);
            std::fs::rename(&path, root.path().join("original")).unwrap();
            let mut replacement = crate::native::create_private_new(&path).unwrap();
            replacement.write_all(b"foreign sentinel").unwrap();
            assert!(identity(&replacement).unwrap() != created);
            drop(replacement);
            let registered = owned.register_file(Path::new(name), created);
            let cleanup = owned.cleanup();
            assert_eq!(
                std::fs::read(&path).ok().as_deref(),
                Some(b"foreign sentinel".as_slice())
            );
            assert!(registered.is_err());
            assert_eq!(cleanup.unwrap_err(), CLEANUP);
        }
    }

    #[test]
    fn reservation_cleans_only_registered_entries_and_retention_keeps_the_graph() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("download");
        let mut owned = OwnedDownload::reserve(&destination, &root.path().join("output")).unwrap();
        owned.create_dir(Path::new("raw")).unwrap();
        file(&owned, "raw/original", b"source");
        file(&owned, "input.m3u8", b"#EXTM3U\n");
        owned.check_identity().unwrap();
        owned.cleanup().unwrap();
        assert!(!destination.exists());

        let mut owned = OwnedDownload::reserve(&destination, &root.path().join("output")).unwrap();
        file(&owned, "input.m3u8", b"retained");
        owned.retain();
        drop(owned);
        assert_eq!(
            std::fs::read(destination.join("input.m3u8")).unwrap(),
            b"retained"
        );
    }

    #[test]
    fn existing_directories_and_unregistered_payloads_are_never_adopted_or_deleted() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("download");
        let mut owned = OwnedDownload::reserve(&destination, &root.path().join("output")).unwrap();
        std::fs::write(destination.join("unregistered"), b"sentinel").unwrap();
        assert_eq!(
            owned.cleanup().unwrap_err().0,
            "download-cleanup-unconfirmed"
        );
        drop(owned);
        assert_eq!(
            std::fs::read(destination.join("unregistered")).unwrap(),
            b"sentinel"
        );
        assert!(matches!(
            OwnedDownload::reserve(&destination, &root.path().join("output")),
            Err(ConversionError("download-directory-exists"))
        ));
        assert_eq!(
            std::fs::read(destination.join("unregistered")).unwrap(),
            b"sentinel"
        );
    }

    #[test]
    fn existing_and_late_output_destinations_are_refused_without_changing_them() {
        let root = tempfile::tempdir().unwrap();
        let download = root.path().join("download");
        let output = root.path().join("output");
        std::fs::write(&output, b"sentinel").unwrap();
        assert!(matches!(
            OwnedDownload::reserve(&download, &output),
            Err(ConversionError("output-exists-or-unsafe"))
        ));
        assert!(!download.exists());
        assert_eq!(std::fs::read(&output).unwrap(), b"sentinel");
        std::fs::remove_file(&output).unwrap();
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("sentinel"), b"directory sentinel").unwrap();
        assert!(matches!(
            OwnedDownload::reserve(&download, &output),
            Err(ConversionError("output-exists-or-unsafe"))
        ));
        assert!(!download.exists());
        assert_eq!(
            std::fs::read(output.join("sentinel")).unwrap(),
            b"directory sentinel"
        );
        std::fs::remove_file(output.join("sentinel")).unwrap();
        std::fs::remove_dir(&output).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path().join("absent-target"), &output).unwrap();
            assert!(matches!(
                OwnedDownload::reserve(&download, &output),
                Err(OUTPUT_EXISTS)
            ));
            assert!(!download.exists());
            assert!(
                std::fs::symlink_metadata(&output)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            std::fs::remove_file(&output).unwrap();
        }
        let mut owned = OwnedDownload::reserve(&download, &output).unwrap();
        owned.check_output(&output).unwrap();
        std::fs::write(&output, b"late sentinel").unwrap();
        assert_eq!(
            owned.check_output(&output).unwrap_err().0,
            "output-exists-or-unsafe"
        );
        owned.cleanup().unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"late sentinel");
    }

    #[test]
    fn replaced_registered_files_and_unowned_parents_are_preserved() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("download");
        let mut owned = OwnedDownload::reserve(&destination, &root.path().join("output")).unwrap();
        file(&owned, "entry", b"owned");
        std::fs::rename(destination.join("entry"), destination.join("old-entry")).unwrap();
        crate::native::create_private_new(&destination.join("entry"))
            .unwrap()
            .write_all(b"owned")
            .unwrap();
        // Equal bytes and valid private permissions still describe a different object.
        assert!(owned.check_registered().is_err());
        assert_eq!(
            owned.cleanup().unwrap_err().0,
            "download-cleanup-unconfirmed"
        );
        assert_eq!(std::fs::read(destination.join("entry")).unwrap(), b"owned");
        assert_eq!(
            std::fs::read(destination.join("old-entry")).unwrap(),
            b"owned"
        );
        assert!(
            owned
                .register_file(Path::new("../outside"), owned.root_id)
                .is_err()
        );
        crate::native::create_private_dir_new(&destination.join("unowned")).unwrap();
        crate::native::create_private_new(&destination.join("unowned/file")).unwrap();
        assert!(
            owned
                .register_file(Path::new("unowned/file"), owned.root_id)
                .is_err()
        );
    }

    #[test]
    fn missing_immediate_parents_are_not_created_and_japanese_distinct_paths_work() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing/download");
        assert!(OwnedDownload::reserve(&missing, &root.path().join("output")).is_err());
        assert!(!root.path().join("missing").exists());
        let mut owned =
            OwnedDownload::reserve(&root.path().join("取得済み"), &root.path().join("変換結果"))
                .unwrap();
        owned.check_identity().unwrap();
        owned.cleanup().unwrap();
        let source_parent = root.path().join("保存先");
        let output_parent = root.path().join("変換先");
        std::fs::create_dir(&source_parent).unwrap();
        std::fs::create_dir(&output_parent).unwrap();
        let mut owned =
            OwnedDownload::reserve(&source_parent.join("入力"), &output_parent.join("出力"))
                .unwrap();
        owned.check_identity().unwrap();
        owned.cleanup().unwrap();
    }

    #[test]
    fn replacing_the_reserved_root_never_deletes_the_replacement_tree() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("download");
        let moved = root.path().join("moved");
        let mut owned = OwnedDownload::reserve(&destination, &root.path().join("output")).unwrap();
        file(&owned, "original", b"original");
        #[cfg(unix)]
        {
            std::fs::rename(&destination, &moved).unwrap();
            std::fs::create_dir(&destination).unwrap();
            std::fs::write(destination.join("sentinel"), b"replacement").unwrap();
            assert!(owned.check_identity().is_err());
            assert_eq!(
                owned.cleanup().unwrap_err().0,
                "download-cleanup-unconfirmed"
            );
            drop(owned);
            assert_eq!(
                std::fs::read(destination.join("sentinel")).unwrap(),
                b"replacement"
            );
            assert_eq!(std::fs::read(moved.join("original")).unwrap(), b"original");
        }
        #[cfg(windows)]
        {
            assert!(std::fs::rename(&destination, &moved).is_err());
            owned.check_identity().unwrap();
            assert_eq!(
                std::fs::read(destination.join("original")).unwrap(),
                b"original"
            );
            owned.cleanup().unwrap();
        }
    }

    #[test]
    fn replacing_a_registered_directory_does_not_redirect_cleanup_or_registration() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("download");
        let mut owned = OwnedDownload::reserve(&destination, &root.path().join("output")).unwrap();
        owned.create_dir(Path::new("raw")).unwrap();
        file(&owned, "raw/original", b"original");
        std::fs::rename(destination.join("raw"), destination.join("old-raw")).unwrap();
        crate::native::create_private_dir_new(&destination.join("raw")).unwrap();
        crate::native::create_private_new(&destination.join("raw/replacement")).unwrap();
        assert!(
            owned
                .register_file(Path::new("raw/replacement"), owned.root_id)
                .is_err()
        );
        assert_eq!(
            owned.cleanup().unwrap_err().0,
            "download-cleanup-unconfirmed"
        );
        drop(owned);
        assert!(destination.join("raw/replacement").is_file());
        assert_eq!(
            std::fs::read(destination.join("old-raw/original")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn current_volume_case_and_unicode_aliases_use_actual_filesystem_identity() {
        let root = tempfile::tempdir().unwrap();
        for (download, alias) in [("Download", "download"), ("Caf\u{e9}", "Cafe\u{301}")] {
            let path = root.path().join(download);
            let other = root.path().join(alias);
            // The volume, rather than a manual case/Unicode fold, decides aliasing.
            let probe = OwnedDownload::reserve(&path, &root.path().join("unrelated")).unwrap();
            let aliases = other.exists();
            drop(probe);
            let result = OwnedDownload::reserve(&path, &other);
            if aliases {
                assert!(matches!(
                    result,
                    Err(ConversionError("download-output-overlap"))
                ));
                assert!(!path.exists());
            } else {
                let mut owned = result.unwrap();
                owned.cleanup().unwrap();
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn native_short_parent_aliases_are_reconciled_when_the_volume_provides_them() {
        use std::os::windows::ffi::OsStringExt as _;
        use std::os::windows::process::CommandExt as _;
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("Long Download Parent Name");
        std::fs::create_dir(&parent).unwrap();
        let long = drmr_test_support::command_path(&parent).unwrap();
        let command = || {
            let mut command = std::process::Command::new("cmd");
            command
                .args(["/D", "/Q", "/U", "/C"])
                .env("DRMR_TEST_PARENT", &long);
            command
        };
        let body = "for %I in (\"%DRMR_TEST_PARENT%\") do @echo %~sI";
        // cmd.exe parses its own command grammar, not C run-time argument escaping.
        // Keep the former encoding as a diagnostic control, without requiring a
        // future Rust toolchain to reproduce its invalid-directory result.
        let previous = command().arg(body).output().unwrap();
        let result = command().raw_arg(body).output().unwrap();
        assert!(result.status.success());
        assert!(result.stdout.len().is_multiple_of(2));
        let decode = |bytes: &[u8]| {
            let mut units: Vec<_> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|v| u16::from_le_bytes([v[0], v[1]]))
                .collect();
            while units.last().is_some_and(|value| matches!(*value, 10 | 13)) {
                units.pop();
            }
            PathBuf::from(std::ffi::OsString::from_wide(&units))
        };
        let alias = decode(&result.stdout);
        eprintln!(
            "download-short-parent-control: old_status_success={} old_utf16_even={} old_stderr_empty={} old_directory={} raw_directory={}",
            previous.status.success(),
            previous.stdout.len().is_multiple_of(2),
            previous.stderr.is_empty(),
            previous.stdout.len().is_multiple_of(2) && decode(&previous.stdout).is_dir(),
            alias.is_dir(),
        );
        assert!(alias.is_dir());
        if alias == long {
            eprintln!("download fixture: volume did not provide a distinct short parent name");
        } else {
            assert!(matches!(
                OwnedDownload::reserve(&parent.join("new-download"), &alias.join("new-download")),
                Err(ConversionError("download-output-overlap"))
            ));
            assert!(!parent.join("new-download").exists());
        }
    }

    #[cfg(unix)]
    #[test]
    fn original_parent_symlink_dotdot_semantics_are_not_lexically_erased() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("actual/child")).unwrap();
        symlink(root.path().join("actual/child"), root.path().join("alias")).unwrap();
        let requested = root.path().join("alias/../download");
        let mut owned = OwnedDownload::reserve(&requested, &root.path().join("output")).unwrap();
        assert_eq!(
            owned.path(),
            std::fs::canonicalize(root.path())
                .unwrap()
                .join("actual/download")
        );
        assert!(!root.path().join("download").exists());
        owned.cleanup().unwrap();
    }
}
