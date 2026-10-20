//! Local media staging and atomic publication without destination replacement.

use super::{ConversionError, MAX_MEDIA_BYTES};
use crate::native;
use std::{
    fs::{File, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};

mod publication;
pub(super) use publication::PublicationSource;
#[cfg(unix)]
mod unix_cleanup;
#[cfg(unix)]
pub(in crate::conversion) use unix_cleanup::check_workspace_parent;

const IO_ERROR: ConversionError = ConversionError("temporary-file-unavailable");
const PUBLISH_ERROR: ConversionError = ConversionError("output-exists-or-publish-failed");
#[cfg(unix)]
const CLEANUP_ERROR: ConversionError = ConversionError("temporary-cleanup-unconfirmed");

/// Ownership protection from stage creation through every unpublished helper failure.
pub(super) struct UnpublishedStage(Stage);
impl UnpublishedStage {
    pub(super) fn new(parent: Option<&Path>) -> Result<Self, ConversionError> {
        Ok(Self(Stage::new(parent)?))
    }
}
impl std::ops::Deref for UnpublishedStage {
    type Target = Stage;
    fn deref(&self) -> &Stage {
        &self.0
    }
}
impl Drop for UnpublishedStage {
    fn drop(&mut self) {
        self.0.preserve_replaced_workspace();
    }
}

pub(super) struct Stage {
    path: PathBuf,
    media: File,
    media_identity: native::FileIdentity,
    #[cfg(windows)]
    root: File,
    #[cfg(unix)]
    outer: UnixOuter,
    // Fields drop in declaration order: remove only the exclusive child while
    // the strict container and all of its ancestors remain pinned.
    #[cfg(windows)]
    owned_child: native::PrivateTempDir,
    #[cfg(windows)]
    _container: Container,
}
impl Stage {
    pub(super) fn new(parent: Option<&Path>) -> Result<Self, ConversionError> {
        #[cfg(windows)]
        {
            let parent = parent.map_or_else(std::env::temp_dir, Path::to_path_buf);
            let mut random = [0; 16];
            getrandom::fill(&mut random)
                .map_err(|_| ConversionError("secure-random-unavailable"))?;
            let name = format!(".drmr-convert-{:032x}", u128::from_be_bytes(random));
            Self::from_container(&parent.join(name))
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut builder = tempfile::Builder::new();
            builder
                .prefix(".drmr-convert-")
                .permissions(std::fs::Permissions::from_mode(0o700));
            let parent = unix_cleanup::Parent::open(
                &parent.map_or_else(std::env::temp_dir, Path::to_path_buf),
            )?;
            let outer = builder
                .tempdir_in(parent.path())
                .map_err(|_| ConversionError("temporary-workspace-unavailable"))?;
            let outer = UnixOuter::from_parent(outer, parent)?;
            let path = outer.path().join("media");
            let media = publication::create_media(&outer.held)?;
            let media_identity = native::FileIdentity::read(&media).map_err(|_| IO_ERROR)?;
            Ok(Self {
                path,
                media,
                media_identity,
                outer,
            })
        }
    }
    #[cfg(windows)]
    fn from_container(path: &Path) -> Result<Self, ConversionError> {
        Self::inside_container(Container::acquire(path)?)
    }
    #[cfg(windows)]
    fn inside_container(container: Container) -> Result<Self, ConversionError> {
        let owned_child =
            native::private_tempdir_in(&container.path, "stage-").map_err(|_| IO_ERROR)?;
        let root = publication::open_directory(owned_child.path(), false)?;
        let path = owned_child.path().join("media");
        create_dir(&path)?;
        let media = publication::open_directory(&path, true)?;
        let media_identity = native::FileIdentity::read(&media).map_err(|_| IO_ERROR)?;
        Ok(Self {
            path,
            media,
            media_identity,
            root,
            owned_child,
            _container: container,
        })
    }
    /// The exclusively owned strict root for separately staged children.
    pub(super) fn private_root(&self) -> &Path {
        #[cfg(unix)]
        {
            self.outer.path()
        }
        #[cfg(windows)]
        {
            self.owned_child.path()
        }
    }
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Opt out of recursive cleanup when an unpublished helper's root was replaced.
    /// Descriptors still close normally; uncertain filesystem entries are retained.
    pub(super) fn preserve_replaced_workspace(&mut self) {
        if self.directory_source().is_ok() {
            return;
        }
        #[cfg(unix)]
        self.outer.preserve();
        #[cfg(windows)]
        self.owned_child.preserve();
    }
}

#[cfg(unix)]
struct UnixOuter {
    path: PathBuf,
    name: std::ffi::OsString,
    parent: unix_cleanup::Parent,
    directory: std::sync::Mutex<Option<tempfile::TempDir>>,
    held: File,
    identity: native::FileIdentity,
}
#[cfg(unix)]
impl UnixOuter {
    #[cfg(test)]
    fn new(directory: tempfile::TempDir) -> Result<Self, ConversionError> {
        let parent = directory
            .path()
            .parent()
            .ok_or(CLEANUP_ERROR)
            .and_then(unix_cleanup::Parent::open);
        match parent {
            Ok(parent) => Self::from_parent(directory, parent),
            Err(error) => {
                let _ = directory.keep();
                Err(error)
            }
        }
    }
    fn from_parent(
        directory: tempfile::TempDir,
        parent: unix_cleanup::Parent,
    ) -> Result<Self, ConversionError> {
        let acquired = (|| {
            parent.check()?;
            let name = directory
                .path()
                .file_name()
                .ok_or(CLEANUP_ERROR)?
                .to_owned();
            let held = parent.open_owned(&name)?;
            let identity = native::FileIdentity::read(&held).map_err(|_| CLEANUP_ERROR)?;
            Ok((name, held, identity))
        })();
        let (name, held, identity) = match acquired {
            Ok(acquired) => acquired,
            Err(error) => {
                let _ = directory.keep();
                return Err(error);
            }
        };
        let outer = Self {
            path: parent.path().join(&name),
            name,
            parent,
            directory: std::sync::Mutex::new(Some(directory)),
            held,
            identity,
        };
        outer.check()?;
        Ok(outer)
    }
    fn path(&self) -> &Path {
        &self.path
    }
    fn check(&self) -> Result<(), ConversionError> {
        self.parent.check()?;
        check_stage_directory(&self.held)?;
        if native::FileIdentity::read(&self.held).map_err(|_| CLEANUP_ERROR)? != self.identity
            || native::FileIdentity::read(&open_stage_directory(&self.path)?)
                .map_err(|_| CLEANUP_ERROR)?
                != self.identity
        {
            return Err(CLEANUP_ERROR);
        }
        Ok(())
    }
    fn cleanup(&self) -> Result<(), ConversionError> {
        self.cleanup_observed(|| {})
    }
    fn preserve(&self) {
        let directory = match self.directory.lock() {
            Ok(mut directory) => directory.take(),
            Err(error) => error.into_inner().take(),
        };
        if let Some(directory) = directory {
            let _ = directory.keep();
        }
    }
    fn cleanup_observed(&self, after_check: impl FnOnce()) -> Result<(), ConversionError> {
        let (directory, poisoned) = match self.directory.lock() {
            Ok(mut directory) => (directory.take(), false),
            Err(error) => (error.into_inner().take(), true),
        };
        let Some(directory) = directory else {
            return Ok(());
        };
        // Disable TempDir's pathname-based Drop before checking the current namespace.
        // Only the retained root descriptor may be walked; uncertain names are kept.
        let _ = directory.keep();
        if poisoned {
            return Err(CLEANUP_ERROR);
        }
        self.check()?;
        after_check();
        unix_cleanup::clear_owned(&self.held)?;
        self.check()?;
        self.parent.remove_owned(&self.name, &self.held)
    }
}

#[cfg(all(test, unix))]
#[path = "files/unix_cleanup_tests.rs"]
mod unix_cleanup_tests;
#[cfg(unix)]
impl Drop for UnixOuter {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
#[cfg(unix)]
fn open_stage_directory(path: &Path) -> Result<File, ConversionError> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        )
        .open(path)
        .map_err(|_| CLEANUP_ERROR)?;
    check_stage_directory(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn check_stage_directory(file: &File) -> Result<(), ConversionError> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = file.metadata().map_err(|_| CLEANUP_ERROR)?;
    if !metadata.is_dir()
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.mode() & 0o077 != 0
    {
        return Err(CLEANUP_ERROR);
    }
    Ok(())
}

#[cfg(windows)]
struct Container {
    path: PathBuf,
    held: Vec<File>,
}
#[cfg(windows)]
impl Container {
    fn acquire(path: &Path) -> Result<Self, ConversionError> {
        let path = checked_absolute(path)?;
        let parent = path.parent().ok_or(IO_ERROR)?;
        let _parents = held_directories(parent)?;
        // Creation installs the strict owner-only ACL in the creation syscall.
        // An existing container is verified without adopting any of its entries.
        native::create_private_dir_all(&path).map_err(|_| IO_ERROR)?;
        // PrivateDir permits deletion sharing. Retain separate no-delete handles
        // and revalidate the current pathname before creating any payload.
        let held = held_directories(&path)?;
        drop(private_fs::PrivateDir::open(&path).map_err(|_| IO_ERROR)?);
        Ok(Self { path, held })
    }
}
#[cfg(windows)]
impl Drop for Container {
    fn drop(&mut self) {
        self.held.clear();
        // Never recursively remove this ensured container: it may have existed
        // before acquisition. Only an empty directory may be removed here.
        let _ = std::fs::remove_dir(&self.path);
    }
}

pub(super) fn create_dir(path: &Path) -> Result<(), ConversionError> {
    native::create_media_dir_all(path).map_err(|_| IO_ERROR)
}
pub(super) fn create_new(path: &Path) -> Result<File, ConversionError> {
    native::create_media_new(path).map_err(|_| IO_ERROR)
}
pub(super) fn seal(path: &Path) -> Result<(), ConversionError> {
    native::seal_media_tree(path).map_err(|_| ConversionError("private-file-permissions-failed"))
}

pub(super) fn regular_input(path: &Path) -> Result<File, ConversionError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        );
    }
    #[cfg(windows)]
    let _parents = {
        use std::os::windows::fs::OpenOptionsExt as _;
        let absolute = checked_absolute(path)?;
        let parents = held_directories(
            absolute
                .parent()
                .ok_or(ConversionError("input-unreadable"))?,
        )?;
        options
            .share_mode(1)
            .custom_flags(0x0020_0000)
            .security_qos_flags(0x0001_0000);
        parents
    };
    let file = options
        .open(path)
        .map_err(|_| ConversionError("input-unreadable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| ConversionError("input-unreadable"))?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(ConversionError("input-not-regular-or-too-large"));
        }
    }
    if !metadata.is_file() || metadata.len() > MAX_MEDIA_BYTES {
        return Err(ConversionError("input-not-regular-or-too-large"));
    }
    Ok(file)
}

pub(super) fn tool_path(path: &Path) -> Result<String, ConversionError> {
    crate::manifest_support::path::fspath(path).map_err(|_| ConversionError("unsupported-path"))
}

pub(super) fn read_manifest(path: &Path) -> Result<Vec<u8>, ConversionError> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    regular_input(path)?
        .take(crate::inspection::MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ConversionError("manifest-unreadable"))?;
    if bytes.len() as u64 > crate::inspection::MAX_MANIFEST_BYTES {
        return Err(ConversionError("manifest-size-limit"));
    }
    Ok(bytes)
}

#[cfg(windows)]
pub(super) fn check_destination(path: &Path) -> Result<(), ConversionError> {
    let absolute = checked_absolute(path)?;
    held_directories(
        absolute
            .parent()
            .ok_or(ConversionError("output-path-invalid"))?,
    )?;
    Ok(())
}

#[cfg(windows)]
fn checked_absolute(path: &Path) -> Result<PathBuf, ConversionError> {
    use std::path::{Component, Prefix};
    let absolute = std::path::absolute(path).map_err(|_| ConversionError("unsupported-path"))?;
    if !matches!(absolute.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
    {
        return Err(ConversionError("unsupported-path"));
    }
    for part in absolute.components() {
        if let Component::Normal(name) = part {
            native::validate_windows_relative_path(
                name.to_str().ok_or(ConversionError("unsupported-path"))?,
            )
            .map_err(|_| ConversionError("unsupported-path"))?;
        }
    }
    Ok(absolute)
}

#[cfg(windows)]
fn held_directories(path: &Path) -> Result<Vec<File>, ConversionError> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::path::Component;
    let absolute = checked_absolute(path)?;
    let mut prefix = PathBuf::new();
    let mut held = Vec::new();
    for part in absolute.components() {
        prefix.push(part);
        if matches!(part, Component::Prefix(_)) {
            continue;
        }
        // FILE_LIST_DIRECTORY participates in sharing checks. Attribute-only
        // access is exempt and would permit a concurrent rename despite the
        // absence of FILE_SHARE_DELETE.
        let directory = OpenOptions::new()
            .access_mode(0x81)
            // Renaming a child can require opening its parent for write access.
            // Preserve that access while still refusing FILE_SHARE_DELETE.
            .share_mode(3)
            .custom_flags(0x0220_0000)
            .security_qos_flags(0x0001_0000)
            .open(&prefix)
            .map_err(|_| ConversionError("unsafe-local-path"))?;
        let metadata = directory
            .metadata()
            .map_err(|_| ConversionError("unsafe-local-path"))?;
        if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
            return Err(ConversionError("unsafe-local-path"));
        }
        held.push(directory);
    }
    Ok(held)
}

pub(super) fn publish_file(
    source: &Path,
    destination: &Path,
    stop: Option<&crate::concurrency::Stop>,
) -> Result<(), ConversionError> {
    publish_file_checked(source, destination, stop, || Ok(()))
}

pub(super) fn publish_file_checked(
    source: &Path,
    destination: &Path,
    stop: Option<&crate::concurrency::Stop>,
    check: impl FnOnce() -> Result<(), ConversionError>,
) -> Result<(), ConversionError> {
    publish_file_observed(source, destination, stop, |_| {}, check)
}

pub(super) fn publish_file_observed(
    source: &Path,
    destination: &Path,
    stop: Option<&crate::concurrency::Stop>,
    after_copy: impl FnOnce(&Path),
    check: impl FnOnce() -> Result<(), ConversionError>,
) -> Result<(), ConversionError> {
    publish_file_with_commit(
        || regular_input(source),
        destination,
        stop,
        after_copy,
        check,
        commit_created_file,
    )
}

fn commit_created_file(
    source: &PublicationSource<'_>,
    destination: &Path,
) -> Result<(), ConversionError> {
    source.commit_to_path(destination)
}

pub(super) fn publish_file_committing(
    source: &PublicationSource<'_>,
    destination: &Path,
    stop: Option<&crate::concurrency::Stop>,
    check: impl FnOnce() -> Result<(), ConversionError>,
    commit: impl FnOnce(&PublicationSource<'_>, &Path) -> Result<(), ConversionError>,
) -> Result<(), ConversionError> {
    publish_file_with_commit(|| source.reader(), destination, stop, |_| {}, check, commit)
}

fn publish_file_with_commit(
    input: impl FnOnce() -> Result<File, ConversionError>,
    destination: &Path,
    stop: Option<&crate::concurrency::Stop>,
    after_copy: impl FnOnce(&Path),
    check: impl FnOnce() -> Result<(), ConversionError>,
    commit: impl FnOnce(&PublicationSource<'_>, &Path) -> Result<(), ConversionError>,
) -> Result<(), ConversionError> {
    check_cancelled(stop)?;
    let parent = destination.parent().ok_or(PUBLISH_ERROR)?;
    let stage = Stage::new(Some(parent))?;
    let result = (|| {
        let path = stage.path().join("verified-output");
        let mut input = input()?;
        let mut output = stage.create_file(&path)?;
        std::io::copy(&mut input, &mut output).map_err(|_| PUBLISH_ERROR)?;
        output.flush().map_err(|_| PUBLISH_ERROR)?;
        output.sync_all().map_err(|_| PUBLISH_ERROR)?;
        let source = stage.created_file_source(&path, &output)?;
        drop(output);
        after_copy(&path);
        #[cfg(unix)]
        stage.outer.check()?;
        seal(stage.path())?;
        check_cancelled(stop)?;
        check()?;
        commit(&source, destination)
    })();
    #[cfg(unix)]
    stage.outer.cleanup()?;
    result
}

pub(super) fn publish_directory(
    source: &Path,
    destination: &Path,
    stop: Option<&crate::concurrency::Stop>,
) -> Result<(), ConversionError> {
    publish_directory_checked(source, destination, stop, || Ok(()))
}

pub(super) fn publish_directory_checked(
    source: &Path,
    destination: &Path,
    stop: Option<&crate::concurrency::Stop>,
    check: impl FnOnce() -> Result<(), ConversionError>,
) -> Result<(), ConversionError> {
    publish_directory_observed(source, destination, stop, |_| {}, check)
}

pub(super) fn publish_directory_observed(
    source: &Path,
    destination: &Path,
    stop: Option<&crate::concurrency::Stop>,
    before_seal: impl FnOnce(&Path),
    check: impl FnOnce() -> Result<(), ConversionError>,
) -> Result<(), ConversionError> {
    publish_directory_with_commit(source, destination, stop, before_seal, check, atomic_move)
}

pub(super) fn publish_directory_committing(
    source: &PublicationSource<'_>,
    destination: &Path,
    stop: Option<&crate::concurrency::Stop>,
    check: impl FnOnce() -> Result<(), ConversionError>,
    commit: impl FnOnce(&PublicationSource<'_>, &Path) -> Result<(), ConversionError>,
) -> Result<(), ConversionError> {
    check_cancelled(stop)?;
    seal(source.path())?;
    check_cancelled(stop)?;
    check()?;
    commit(source, destination)
}

fn publish_directory_with_commit(
    source: &Path,
    destination: &Path,
    stop: Option<&crate::concurrency::Stop>,
    before_seal: impl FnOnce(&Path),
    check: impl FnOnce() -> Result<(), ConversionError>,
    commit: impl FnOnce(&Path, &Path) -> Result<(), ConversionError>,
) -> Result<(), ConversionError> {
    check_cancelled(stop)?;
    before_seal(source);
    seal(source)?;
    check_cancelled(stop)?;
    check()?;
    commit(source, destination)
}
fn check_cancelled(stop: Option<&crate::concurrency::Stop>) -> Result<(), ConversionError> {
    if stop.is_some_and(crate::concurrency::Stop::is_set) {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod unix_stage_identity_tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    #[test]
    fn unix_outer_refuses_a_writable_replacement_before_acquisition() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("shared");
        std::fs::create_dir(&parent).unwrap();
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o777)).unwrap();
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(&parent)
            .unwrap();
        let path = directory.path().to_path_buf();
        let original = root.path().join("original");
        std::fs::write(path.join("owned"), b"owned stage bytes").unwrap();
        std::fs::rename(&path, &original).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
        std::fs::write(path.join("foreign"), b"foreign bytes must remain").unwrap();
        // A same-user test exercises the unsafe-mode branch without requiring
        // privilege to create a directory owned by another user.
        assert_eq!(
            std::fs::metadata(&path).unwrap().uid(),
            rustix::process::geteuid().as_raw()
        );
        let result = UnixOuter::new(directory);
        let rejected = result.is_err();
        drop(result);
        assert!(
            rejected,
            "a writable replacement was adopted; foreign preserved: {}",
            path.join("foreign").exists()
        );
        assert_eq!(
            std::fs::read(original.join("owned")).unwrap(),
            b"owned stage bytes"
        );
        assert_eq!(
            std::fs::read(path.join("foreign")).unwrap(),
            b"foreign bytes must remain"
        );
        assert_eq!(
            std::fs::metadata(&original).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o777
        );
    }

    #[test]
    fn unix_outer_refuses_privacy_changes_after_acquisition_without_repair_or_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let directory = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(root.path())
            .unwrap();
        let outer = UnixOuter::new(directory).unwrap();
        let path = outer.path().to_path_buf();
        std::fs::write(path.join("owned"), b"retained after privacy change").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
        let result = outer.check();
        drop(outer);
        assert_eq!(result, Err(CLEANUP_ERROR));
        assert_eq!(
            std::fs::read(path.join("owned")).unwrap(),
            b"retained after privacy change"
        );
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o777
        );
    }

    #[test]
    fn stage_drop_preserves_a_foreign_tree_after_its_parent_is_replaced() {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let stage = Stage::new(Some(&work)).unwrap();
        let leaf = stage.private_root().file_name().unwrap().to_owned();
        create_new(&stage.path().join("owned"))
            .unwrap()
            .write_all(b"owned stage")
            .unwrap();
        std::fs::rename(&work, root.path().join("moved-work")).unwrap();
        std::fs::create_dir(&work).unwrap();
        let foreign = work.join(&leaf);
        std::fs::create_dir(&foreign).unwrap();
        std::fs::write(foreign.join("foreign-sentinel"), b"must remain").unwrap();
        drop(stage);
        assert_eq!(
            std::fs::read(foreign.join("foreign-sentinel")).unwrap(),
            b"must remain"
        );
        assert_eq!(
            std::fs::read(
                root.path()
                    .join("moved-work")
                    .join(leaf)
                    .join("media/owned")
            )
            .unwrap(),
            b"owned stage"
        );
    }

    #[test]
    fn uncertain_stage_cleanup_overrides_cancellation_without_deleting_a_replacement() {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let original = root.path().join("candidate");
        std::fs::write(&original, b"verified candidate").unwrap();
        let stop = crate::concurrency::Stop::new();
        let mut leaf = None;
        let result = publish_file_observed(
            &original,
            &work.join("result"),
            Some(&stop),
            |path| {
                let name = path
                    .parent()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_owned();
                std::fs::rename(&work, root.path().join("moved-work")).unwrap();
                std::fs::create_dir(&work).unwrap();
                std::fs::create_dir(work.join(&name)).unwrap();
                std::fs::write(work.join(&name).join("foreign-sentinel"), b"must remain").unwrap();
                leaf = Some(name);
                stop.set();
            },
            || Ok(()),
        );
        assert_eq!(result.unwrap_err(), CLEANUP_ERROR);
        let leaf = leaf.unwrap();
        assert_eq!(
            std::fs::read(work.join(&leaf).join("foreign-sentinel")).unwrap(),
            b"must remain"
        );
        assert_eq!(
            std::fs::read(
                root.path()
                    .join("moved-work")
                    .join(leaf)
                    .join("media/verified-output")
            )
            .unwrap(),
            b"verified candidate"
        );
        assert!(!work.join("result").exists());
        assert_eq!(std::fs::read(&original).unwrap(), b"verified candidate");
    }
}

pub(super) fn atomic_move(source: &Path, destination: &Path) -> Result<(), ConversionError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            source,
            rustix::fs::CWD,
            destination,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|_| PUBLISH_ERROR)
    }
    #[cfg(windows)]
    {
        // Zero flags forbids replacement and cross-volume copy fallback. Staging
        // lives below the destination parent, so the existing ACL moves with it.
        winsafe::MoveFileEx(
            source.to_str().ok_or(PUBLISH_ERROR)?,
            Some(destination.to_str().ok_or(PUBLISH_ERROR)?),
            winsafe::co::MOVEFILE::default(),
        )
        .map_err(|_| PUBLISH_ERROR)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (source, destination);
        Err(ConversionError("private-files-not-supported-on-platform"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn strict_stage_birth_does_not_depend_on_the_callers_umask() {
        use std::os::unix::fs::PermissionsExt as _;
        const CHILD: &str = "DRMR_STAGE_UMASK_CHILD";
        if std::env::var_os(CHILD).as_deref() == Some(std::ffi::OsStr::new("1")) {
            rustix::process::umask(rustix::fs::Mode::empty());
            let parent = tempfile::Builder::new()
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir()
                .unwrap();
            for location in [Some(parent.path()), None] {
                let stage = Stage::new(location).unwrap();
                assert_eq!(
                    std::fs::metadata(stage.private_root())
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
                assert_eq!(
                    std::fs::metadata(stage.path())
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
            }
            return;
        }
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "conversion::files::tests::strict_stage_birth_does_not_depend_on_the_callers_umask",
            ])
            .env(CHILD, "1")
            .stdout(std::process::Stdio::null());
        let mut child = native::ManagedChild::spawn(command, true).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "isolated umask child did not complete"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        child.finish().unwrap();
        assert!(status.success());
    }

    #[test]
    fn cancellation_after_file_copy_prevents_publication_and_preserves_source() {
        let parent = tempfile::tempdir().unwrap();
        let source = Stage::new(None).unwrap();
        let input = source.path().join("input");
        let bytes = b"verified source bytes";
        create_new(&input).unwrap().write_all(bytes).unwrap();
        let destination = parent.path().join("cancelled-result");
        let stop = crate::concurrency::Stop::new();
        let result = publish_file_observed(
            &input,
            &destination,
            Some(&stop),
            |copied| {
                assert_eq!(std::fs::read(copied).unwrap(), bytes);
                stop.set();
            },
            || Ok(()),
        );
        assert_eq!(result.unwrap_err(), ConversionError("cancelled"));
        assert!(!destination.exists());
        assert_eq!(std::fs::read(&input).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[test]
    fn cancellation_during_directory_preparation_preserves_source_and_publishes_nothing() {
        let parent = tempfile::tempdir().unwrap();
        let source = Stage::new(None).unwrap();
        let input = source.path().join("input");
        let bytes = b"verified directory payload";
        create_new(&input).unwrap().write_all(bytes).unwrap();
        let destination = parent.path().join("cancelled-result");
        let stop = crate::concurrency::Stop::new();
        let result = publish_directory_observed(
            source.path(),
            &destination,
            Some(&stop),
            |directory| {
                assert_eq!(std::fs::read(directory.join("input")).unwrap(), bytes);
                stop.set();
            },
            || Ok(()),
        );
        assert_eq!(result.unwrap_err(), ConversionError("cancelled"));
        assert!(!destination.exists());
        assert_eq!(std::fs::read(&input).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[test]
    fn downloaded_source_mutation_during_file_copy_prevents_atomic_publication() {
        let parent = tempfile::tempdir().unwrap();
        let source = Stage::new(None).unwrap();
        let saved = source.path().join("saved");
        let candidate = source.path().join("candidate");
        create_new(&saved).unwrap().write_all(b"original").unwrap();
        create_new(&candidate)
            .unwrap()
            .write_all(b"verified output")
            .unwrap();
        let output = parent.path().join("result");
        let result = publish_file_observed(
            &candidate,
            &output,
            None,
            |_| {
                std::fs::write(&saved, b"changed").unwrap();
            },
            || {
                if std::fs::read(&saved).unwrap() == b"original" {
                    Ok(())
                } else {
                    Err(ConversionError("download-source-changed"))
                }
            },
        );
        assert_eq!(
            result.unwrap_err(),
            ConversionError("download-source-changed")
        );
        assert!(!output.exists());
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
        assert_eq!(std::fs::read(candidate).unwrap(), b"verified output");
    }

    #[test]
    fn downloaded_source_mutation_during_directory_seal_prevents_atomic_publication() {
        let parent = tempfile::tempdir().unwrap();
        let source = Stage::new(None).unwrap();
        let saved = source.path().join("saved");
        create_new(&saved).unwrap().write_all(b"original").unwrap();
        let output = parent.path().join("result");
        let result = publish_directory_observed(
            source.path(),
            &output,
            None,
            |_| {
                std::fs::write(&saved, b"changed").unwrap();
            },
            || {
                if std::fs::read(&saved).unwrap() == b"original" {
                    Ok(())
                } else {
                    Err(ConversionError("download-source-changed"))
                }
            },
        );
        assert_eq!(
            result.unwrap_err(),
            ConversionError("download-source-changed")
        );
        assert!(!output.exists());
        assert_eq!(std::fs::read(saved).unwrap(), b"changed");
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[test]
    fn staged_payload_is_private_before_publication_and_owned_stage_is_cleaned() {
        let parent = tempfile::tempdir().unwrap();
        let stage = Stage::new(Some(parent.path())).unwrap();
        let directory = stage.path().to_path_buf();
        let file = directory.join("payload");
        let mut held = create_new(&file).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(held.metadata().unwrap().permissions().mode() & 0o077, 0);
            assert_eq!(
                std::fs::metadata(&directory).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        #[cfg(windows)]
        private_fs::verify(&held).unwrap();
        held.write_all(b"private bytes").unwrap();
        drop(held);
        drop(stage);
        assert!(!directory.exists());
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }

    #[test]
    fn directory_publication_is_atomic_and_never_replaces_an_existing_empty_or_populated_destination()
     {
        let parent = tempfile::tempdir().unwrap();
        for populated in [false, true] {
            let stage = Stage::new(Some(parent.path())).unwrap();
            let candidate = stage.path();
            create_new(&candidate.join("media"))
                .unwrap()
                .write_all(b"candidate")
                .unwrap();
            let target = parent
                .path()
                .join(if populated { "populated" } else { "empty" });
            std::fs::create_dir(&target).unwrap();
            if populated {
                std::fs::write(target.join("sentinel"), b"keep").unwrap();
            }
            assert!(publish_directory(candidate, &target, None).is_err());
            assert_eq!(
                std::fs::read(candidate.join("media")).unwrap(),
                b"candidate"
            );
            assert!(!target.join("media").exists());
            if populated {
                assert_eq!(std::fs::read(target.join("sentinel")).unwrap(), b"keep");
            }
        }
        let stage = Stage::new(Some(parent.path())).unwrap();
        create_new(&stage.path().join("media"))
            .unwrap()
            .write_all(b"verified")
            .unwrap();
        let target = parent.path().join("published");
        publish_directory(stage.path(), &target, None).unwrap();
        drop(stage);
        assert_eq!(std::fs::read(target.join("media")).unwrap(), b"verified");
    }

    #[test]
    fn file_publication_preserves_an_existing_file_and_publishes_exact_bytes_otherwise() {
        let parent = tempfile::tempdir().unwrap();
        let stage = Stage::new(None).unwrap();
        let source = stage.path().join("source");
        create_new(&source)
            .unwrap()
            .write_all(b"new bytes")
            .unwrap();
        let target = parent.path().join("result");
        std::fs::write(&target, b"keep").unwrap();
        assert!(publish_file(&source, &target, None).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        std::fs::remove_file(&target).unwrap();
        publish_file(&source, &target, None).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new bytes");
        #[cfg(windows)]
        private_fs::verify(&std::fs::File::open(target).unwrap()).unwrap();
    }
    #[cfg(windows)]
    #[test]
    fn child_publication_requires_write_sharing_without_allowing_parent_replacement() {
        use std::os::windows::fs::OpenOptionsExt as _;
        let root = tempfile::tempdir().unwrap();
        // Pin only this unique parent: the deliberately restrictive control must
        // not interfere with unrelated tests' directory operations on the drive.
        let directory = root.path().join("private-publication");
        native::create_private_dir_all(&directory).unwrap();
        for sharing in [1, 3] {
            let source = directory.join(format!("source-{sharing}"));
            let target = directory.join(format!("target-{sharing}"));
            native::create_private_new(&source)
                .unwrap()
                .write_all(b"verified")
                .unwrap();
            let held = OpenOptions::new()
                .access_mode(0x81)
                .share_mode(sharing)
                .security_qos_flags(0x0001_0000)
                .custom_flags(0x0220_0000)
                .open(&directory)
                .unwrap();
            assert!(
                std::fs::rename(&directory, root.path().join("redirected")).is_err(),
                "both sharing modes must refuse deletion/rename of the pinned parent"
            );
            let moved = winsafe::MoveFileEx(
                source.to_str().unwrap(),
                Some(target.to_str().unwrap()),
                winsafe::co::MOVEFILE::default(),
            );
            if sharing == 1 {
                let error =
                    moved.expect_err("the read-only-sharing control must refuse child publication");
                eprintln!("publication read-only-sharing control WIN32=0x{error:08x}");
                assert_eq!(
                    error,
                    winsafe::co::ERROR::SHARING_VIOLATION,
                    "unexpected native control error: 0x{error:08x}"
                );
                assert_eq!(std::fs::read(&source).unwrap(), b"verified");
                assert!(!target.exists());
            } else {
                moved.unwrap_or_else(|error| {
                    panic!("write-sharing publication failed WIN32=0x{error:08x}")
                });
                assert_eq!(std::fs::read(&target).unwrap(), b"verified");
                assert!(!source.exists());
            }
            drop(held);
        }
        std::fs::rename(&directory, root.path().join("released")).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn empty_pinned_target_replacement_is_observed_and_cannot_admit_a_public_workspace() {
        let parent = tempfile::tempdir().unwrap();
        let user = windows_permissions::utilities::current_process_sid().unwrap();
        set_directory_dacl(
            parent.path(),
            &format!("D:P(A;OICI;FA;;;{user})(A;;0x00000040;;;WD)"),
        );
        let target = parent.path().join("empty-private-container");
        let replacement = parent.path().join("public-replacement");
        std::fs::create_dir(&replacement).unwrap();
        set_directory_dacl(&replacement, "D:P(A;OICI;FA;;;WD)");
        let replacement_identity = same_file::Handle::from_path(&replacement).unwrap();
        let container = Container::acquire(&target).unwrap();
        // Clone the retained target handle; pathname metadata alone cannot prove
        // that a successful target replacement left the same directory in place.
        let original_identity =
            same_file::Handle::from_file(container.held.last().unwrap().try_clone().unwrap())
                .unwrap();
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
        match std::fs::rename(&replacement, &target) {
            Ok(()) => {
                eprintln!(
                    "empty pinned target replacement permitted; workspace admission must fail closed"
                );
                let current = same_file::Handle::from_path(&target).unwrap();
                assert_ne!(current, original_identity);
                assert_eq!(current, replacement_identity);
                std::fs::write(target.join("sentinel"), b"existing-public-content").unwrap();
                assert!(Stage::inside_container(container).is_err());
                assert_eq!(
                    std::fs::read(target.join("sentinel")).unwrap(),
                    b"existing-public-content"
                );
                assert_eq!(
                    std::fs::read_dir(&target).unwrap().count(),
                    1,
                    "refused admission must not create payloads or recursively clean the replacement"
                );
            }
            Err(error) => {
                eprintln!(
                    "empty pinned target replacement denied WIN32={:?}",
                    error.raw_os_error()
                );
                assert_eq!(
                    same_file::Handle::from_path(&target).unwrap(),
                    original_identity
                );
                assert!(replacement.exists());
                drop(original_identity);
                drop(container);
                // Confirm that the filesystem supports replacing an existing
                // empty directory; a platform limitation is not a pin proof.
                native::create_private_dir_all(&target).unwrap();
                assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
                std::fs::rename(&replacement, &target)
                    .expect("unpinned existing empty target replacement control must succeed");
                assert_eq!(
                    same_file::Handle::from_path(&target).unwrap(),
                    replacement_identity
                );
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn stage_container_is_strict_and_pinned_even_when_parent_grants_delete_child() {
        use std::os::windows::fs::OpenOptionsExt as _;
        let parent = tempfile::tempdir().unwrap();
        let user = windows_permissions::utilities::current_process_sid().unwrap();
        set_directory_dacl(
            parent.path(),
            &format!("D:P(A;OICI;FA;;;{user})(A;;0x00000040;;;WD)"),
        );
        // Execute the previous metadata-only sharing mechanism as a control.
        // Its missing LIST_DIRECTORY bit must permit the same rename attack.
        let control_path = parent.path().join("metadata-only-control");
        let control_moved = parent.path().join("metadata-only-moved");
        native::create_private_dir_all(&control_path).unwrap();
        let metadata_only = OpenOptions::new()
            .access_mode(0x80)
            .share_mode(1)
            .security_qos_flags(0x0001_0000)
            .custom_flags(0x0220_0000)
            .open(&control_path)
            .unwrap();
        std::fs::rename(&control_path, &control_moved)
            .expect("metadata-only access must demonstrate the unpinned control");
        std::fs::rename(&control_moved, &control_path).unwrap();
        drop(metadata_only);
        std::fs::remove_dir(&control_path).unwrap();

        let stage = Stage::new(Some(parent.path())).unwrap();
        let container = stage
            .path()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        private_fs::PrivateDir::open(&container).unwrap();
        create_new(&stage.path().join("payload"))
            .unwrap()
            .write_all(b"private")
            .unwrap();
        // Ensured-container entries are never owned by the stage's cleanup.
        native::create_private_new(&container.join("sentinel"))
            .unwrap()
            .write_all(b"keep")
            .unwrap();
        assert!(std::fs::rename(&container, parent.path().join("redirected")).is_err());
        assert_eq!(
            std::fs::read(stage.path().join("payload")).unwrap(),
            b"private"
        );
        let owned_child = stage.path().parent().unwrap().to_path_buf();
        drop(stage);
        assert!(!owned_child.exists());
        let after_drop = parent.path().join("after-drop");
        std::fs::rename(&container, &after_drop)
            .expect("releasing the stage must release its pathname pins");
        assert_eq!(std::fs::read(after_drop.join("sentinel")).unwrap(), b"keep");
        assert!(!container.exists());
    }

    #[cfg(windows)]
    fn set_directory_dacl(path: &Path, policy: &str) {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_permissions::constants::{SeObjectType, SecurityInformation};
        use windows_permissions::wrappers::{
            ConvertSecurityDescriptorToStringSecurityDescriptor, GetSecurityInfo, SetSecurityInfo,
        };
        let policy: windows_permissions::LocalBox<windows_permissions::SecurityDescriptor> =
            policy.parse().unwrap();
        let mut directory = OpenOptions::new()
            // Read back the descriptor independently; inheritance updates and
            // directory enumeration also require the corresponding read rights.
            .access_mode(0x0006_0081)
            .share_mode(7)
            .custom_flags(0x0200_0000)
            .open(path)
            .unwrap();
        SetSecurityInfo(
            &mut directory,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
            None,
            None,
            Some(policy.dacl().unwrap()),
            None,
        )
        .unwrap();
        let installed = GetSecurityInfo(
            &directory,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Dacl,
        )
        .unwrap();
        let dacl_text = |descriptor: &windows_permissions::SecurityDescriptor| {
            ConvertSecurityDescriptorToStringSecurityDescriptor(
                descriptor,
                SecurityInformation::Dacl,
            )
            .unwrap()
            .into_string()
            .unwrap()
        };
        let actual = dacl_text(&installed);
        let expected = dacl_text(&policy);
        let (header, actual_aces) = actual.split_at(actual.find('(').unwrap());
        let expected_aces = &expected[expected.find('(').unwrap()..];
        assert!(
            matches!(header, "D:P" | "D:PAI"),
            "fixture ACL must be protected"
        );
        assert_eq!(
            actual_aces, expected_aces,
            "fixture ACL must retain the exact threat policy"
        );
    }

    #[cfg(windows)]
    #[test]
    fn unsafe_existing_container_is_refused_without_repair_or_cleanup() {
        let parent = tempfile::tempdir().unwrap();
        let container = parent.path().join("unsafe-container");
        std::fs::create_dir(&container).unwrap();
        set_directory_dacl(&container, "D:P(A;OICI;FA;;;WD)");
        std::fs::write(container.join("sentinel"), b"keep").unwrap();
        assert!(private_fs::PrivateDir::open(&container).is_err());
        assert!(Stage::from_container(&container).is_err());
        assert!(private_fs::PrivateDir::open(&container).is_err());
        assert_eq!(std::fs::read(container.join("sentinel")).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(&container).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn stage_cleanup_never_adopts_or_deletes_existing_container_contents() {
        let parent = tempfile::tempdir().unwrap();
        let container = parent.path().join("existing-private-container");
        native::create_private_dir_all(&container).unwrap();
        native::create_private_new(&container.join("sentinel"))
            .unwrap()
            .write_all(b"keep")
            .unwrap();
        let stage = Stage::from_container(&container).unwrap();
        let owned_child = stage.path().parent().unwrap().to_path_buf();
        create_new(&stage.path().join("payload"))
            .unwrap()
            .write_all(b"temporary")
            .unwrap();
        drop(stage);
        assert!(!owned_child.exists());
        assert_eq!(std::fs::read(container.join("sentinel")).unwrap(), b"keep");
        assert_eq!(std::fs::read_dir(&container).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn public_media_refuses_alternate_streams_and_junction_ancestors() {
        let root = tempfile::tempdir().unwrap();
        let public = root.path().join("public");
        std::fs::create_dir(&public).unwrap();
        let media = public.join("media.mp4");
        std::fs::write(&media, b"public media").unwrap();
        assert!(regular_input(&media).is_ok());
        let stream = public.join("media.mp4:secret");
        std::fs::write(&stream, b"hidden stream").unwrap();
        assert!(regular_input(&stream).is_err());
        let alias = root.path().join("junction");
        let status = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(drmr_test_support::command_path(&alias).unwrap())
            .arg(drmr_test_support::command_path(&public).unwrap())
            .status()
            .unwrap();
        assert!(status.success());
        assert!(regular_input(&alias.join("media.mp4")).is_err());
        assert!(check_destination(&alias.join("result.mp4")).is_err());
        assert!(!public.join("result.mp4").exists());
    }
}
