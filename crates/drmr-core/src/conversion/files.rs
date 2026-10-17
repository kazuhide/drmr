//! Local media staging and atomic publication without destination replacement.

use super::{ConversionError, MAX_MEDIA_BYTES};
use crate::native;
use std::{
    fs::{File, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};

const IO_ERROR: ConversionError = ConversionError("temporary-file-unavailable");
const PUBLISH_ERROR: ConversionError = ConversionError("output-exists-or-publish-failed");

pub(super) struct Stage {
    path: PathBuf,
    _outer: tempfile::TempDir,
    #[cfg(windows)]
    _parents: Vec<File>,
}
impl Stage {
    pub(super) fn new(parent: Option<&Path>) -> Result<Self, ConversionError> {
        #[cfg(windows)]
        let parents = parent
            .map(held_directories)
            .transpose()?
            .unwrap_or_default();
        let mut builder = tempfile::Builder::new();
        builder.prefix(".drmr-convert-");
        let outer = match parent {
            Some(parent) => builder.tempdir_in(parent),
            None => builder.tempdir(),
        }
        .map_err(|_| ConversionError("temporary-workspace-unavailable"))?;
        // The exclusive outer directory contains no payload. The inner directory
        // receives an owner-only ACL at creation, before inheritance is enabled.
        let path = outer.path().join("media");
        create_dir(&path)?;
        Ok(Self {
            path,
            _outer: outer,
            #[cfg(windows)]
            _parents: parents,
        })
    }
    pub(super) fn path(&self) -> &Path {
        &self.path
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
        let directory = OpenOptions::new()
            .access_mode(0x80)
            .share_mode(1)
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

pub(super) fn publish_file(source: &Path, destination: &Path) -> Result<(), ConversionError> {
    let parent = destination.parent().ok_or(PUBLISH_ERROR)?;
    let stage = Stage::new(Some(parent))?;
    let path = stage.path().join("verified-output");
    let mut input = regular_input(source)?;
    let mut output = create_new(&path)?;
    std::io::copy(&mut input, &mut output).map_err(|_| PUBLISH_ERROR)?;
    output.flush().map_err(|_| PUBLISH_ERROR)?;
    output.sync_all().map_err(|_| PUBLISH_ERROR)?;
    drop(output);
    seal(stage.path())?;
    atomic_move(&path, destination)
}

pub(super) fn publish_directory(source: &Path, destination: &Path) -> Result<(), ConversionError> {
    seal(source)?;
    atomic_move(source, destination)
}
fn atomic_move(source: &Path, destination: &Path) -> Result<(), ConversionError> {
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

    #[test]
    fn staged_payload_is_private_before_publication_and_cleanup_owns_only_the_outer_directory() {
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
            assert!(publish_directory(candidate, &target).is_err());
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
        publish_directory(stage.path(), &target).unwrap();
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
        assert!(publish_file(&source, &target).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
        std::fs::remove_file(&target).unwrap();
        publish_file(&source, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new bytes");
        #[cfg(windows)]
        private_fs::verify(&std::fs::File::open(target).unwrap()).unwrap();
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
