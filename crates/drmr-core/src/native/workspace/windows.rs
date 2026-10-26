//! Windows workspace ACLs through safe handle-based permission wrappers.

use super::{invalid, walk};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use windows_permissions::constants::{SeObjectType, SecurityInformation};
use windows_permissions::utilities::current_process_sid;
use windows_permissions::wrappers::{
    ConvertSecurityDescriptorToStringSecurityDescriptor, GetSecurityDescriptorOwner,
    GetSecurityInfo, SetSecurityInfo,
};
use windows_permissions::{LocalBox, SecurityDescriptor, Sid};

struct Policy {
    user: LocalBox<Sid>,
    strict: LocalBox<SecurityDescriptor>,
    media: LocalBox<SecurityDescriptor>,
    strict_ace: String,
    media_ace: String,
    inherited_file_ace: String,
    inherited_directory_ace: String,
}

impl Policy {
    fn new() -> io::Result<Self> {
        let user = current_process_sid()?;
        let descriptor = |flags| -> io::Result<LocalBox<SecurityDescriptor>> {
            format!("D:P(A;{flags};FA;;;{user})").parse()
        };
        let strict = descriptor("")?;
        let media = descriptor("OICI")?;
        let inherited_file = descriptor("ID")?;
        let inherited_directory = descriptor("OICIID")?;
        Ok(Self {
            strict_ace: ace_text(&strict)?.1,
            media_ace: ace_text(&media)?.1,
            inherited_file_ace: ace_text(&inherited_file)?.1,
            inherited_directory_ace: ace_text(&inherited_directory)?.1,
            user,
            strict,
            media,
        })
    }

    fn verify(&self, file: &File, directory: bool, protected: bool) -> io::Result<()> {
        let metadata = file.metadata()?;
        if metadata.file_attributes() & 0x400 != 0
            || directory != metadata.is_dir()
            || (!directory && !metadata.is_file())
        {
            return Err(invalid());
        }
        if !directory {
            // Metadata, file identity, owner and single-link checks precede the
            // strict ACL check in private-fs. Only its ACL-policy rejection may
            // be admitted by this separate inherited-media policy.
            match private_fs::verify(file) {
                Ok(()) | Err(private_fs::Error::InsecureAcl) => {}
                Err(_) => return Err(invalid()),
            }
        }
        let descriptor = GetSecurityInfo(
            file,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Owner | SecurityInformation::Dacl,
        )?;
        if GetSecurityDescriptorOwner(&descriptor)? != Some(&*self.user) {
            return Err(invalid());
        }
        let (header, ace) = ace_text(&descriptor)?;
        if !matches!(header.as_str(), "D:" | "D:P" | "D:AI" | "D:PAI")
            || (protected && !matches!(header.as_str(), "D:P" | "D:PAI"))
        {
            return Err(invalid());
        }
        let permitted = ace == self.strict_ace
            || if directory {
                ace == self.media_ace || ace == self.inherited_directory_ace
            } else {
                ace == self.inherited_file_ace
            };
        if !permitted {
            return Err(invalid());
        }
        Ok(())
    }

    fn set(&self, file: &mut File, directory: bool) -> io::Result<()> {
        let descriptor = if directory { &self.media } else { &self.strict };
        let acl = descriptor.dacl().ok_or_else(invalid)?;
        SetSecurityInfo(
            file,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
            None,
            None,
            Some(acl),
            None,
        )?;
        self.verify(file, directory, true)?;
        if !directory {
            private_fs::verify(file).map_err(|_| invalid())?;
        }
        Ok(())
    }
}

// Compare canonical OS-generated ACL text, rather than using an ACE accessor
// that can panic for newer or unknown ACE types in an adversarial descriptor.
fn ace_text(descriptor: &SecurityDescriptor) -> io::Result<(String, String)> {
    let text =
        ConvertSecurityDescriptorToStringSecurityDescriptor(descriptor, SecurityInformation::Dacl)?
            .into_string()
            .map_err(|_| invalid())?;
    let at = text.find('(').ok_or_else(invalid)?;
    Ok((text[..at].to_string(), text[at..].to_string()))
}

fn open(path: &Path) -> io::Result<File> {
    // LIST_DIRECTORY (READ_DATA on a file) makes the handle participate in
    // Windows sharing checks. Metadata-only access cannot pin a directory.
    const READ_CONTROL_WRITE_DAC_READ_DATA_ATTRIBUTES: u32 = 0x0006_0081;
    const BACKUP_SEMANTICS_OPEN_REPARSE_POINT: u32 = 0x0220_0000;
    OpenOptions::new()
        .access_mode(READ_CONTROL_WRITE_DAC_READ_DATA_ATTRIBUTES)
        .share_mode(1)
        .security_qos_flags(0x0001_0000)
        .custom_flags(BACKUP_SEMANTICS_OPEN_REPARSE_POINT)
        .open(path)
}

pub(super) fn pin(path: &Path) -> io::Result<crate::native::PrivateDirectoryGuard> {
    let guard = crate::native::files::pin_directory_path(path)?;
    Policy::new()?.verify(&open(path)?, true, true)?;
    Ok(guard)
}

pub(super) fn create_dir_new(scratch: &Path, path: &Path) -> io::Result<File> {
    create_dir_new_with_hooks(scratch, path, |_| {}, |_| {})
}

#[cfg(test)]
fn create_dir_new_after_creation(
    scratch: &Path,
    path: &Path,
    after_creation: impl FnOnce(&Path),
) -> io::Result<File> {
    create_dir_new_with_hooks(scratch, path, after_creation, |_| {})
}

fn create_dir_new_with_hooks(
    scratch: &Path,
    path: &Path,
    after_creation: impl FnOnce(&Path),
    before_move: impl FnOnce(&Path),
) -> io::Result<File> {
    use std::os::windows::io::AsHandle as _;
    let absolute = std::path::absolute(path)?;
    let parent = absolute.parent().ok_or_else(invalid)?;
    let target_name = absolute.file_name().ok_or_else(invalid)?;
    crate::native::validate_windows_relative_path(target_name.to_str().ok_or_else(invalid)?)?;
    let _ancestors = crate::native::files::pin_directory_path(parent)?;
    let scratch = std::path::absolute(scratch)?;
    let _scratch_ancestors = crate::native::files::pin_directory_path(&scratch)?;
    match std::fs::symlink_metadata(&absolute) {
        Ok(_) => return Err(io::ErrorKind::AlreadyExists.into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let policy = Policy::new()?;
    let parent_handle = open(parent)?;
    policy.verify(&parent_handle, true, true)?;
    let descriptor = GetSecurityInfo(
        &parent_handle,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl,
    )?;
    if ace_text(&descriptor)?.1 != policy.media_ace {
        return Err(invalid());
    }
    let mut random = [0; 16];
    getrandom::fill(&mut random)
        .map_err(|_| io::Error::other("secure random source unavailable"))?;
    let name = format!(".drmr-media-dir-{}", hex::encode(random));
    let creator = private_fs::PrivateDir::open(&scratch).map_err(|_| invalid())?;
    // PrivateDir::create_dir returns the NT creation handle with an owner-only
    // descriptor. Capture its identity before any pathname can be substituted.
    let created = creator.create_dir(&name).map_err(|_| invalid())?;
    let mut child = File::from(created.as_handle().try_clone_to_owned()?);
    let identity = crate::native::FileIdentity::read(&child)?;
    let pending = scratch.join(name);
    after_creation(&pending);
    let current = OpenOptions::new()
        .access_mode(0x81)
        .share_mode(7)
        .custom_flags(0x0220_0000)
        .security_qos_flags(0x0001_0000)
        .open(&pending)?;
    if crate::native::FileIdentity::read(&current)? != identity {
        return Err(invalid());
    }
    drop(current);
    policy.verify(&child, true, true)?;
    before_move(&pending);
    // Rename the created object, even if another entry now has its old name.
    // Keep the validated destination handle and all ancestor pins throughout.
    rename_handle_no_replace(&child, &parent_handle, target_name, true)?;
    policy.set(&mut child, true)?;
    // Creation handles carry DELETE access; close every clone before acquiring
    // the final no-delete pin. A replacement is never adopted or cleaned up.
    drop(created);
    drop(child);
    let retained = open(&absolute)?;
    if crate::native::FileIdentity::read(&retained)? != identity {
        return Err(invalid());
    }
    policy.verify(&retained, true, true)?;
    Ok(retained)
}

pub(super) fn open_mutation(path: &Path, directory: bool) -> io::Result<File> {
    let absolute = std::path::absolute(path)?;
    let parent = absolute.parent().ok_or_else(invalid)?;
    let name = absolute.file_name().ok_or_else(invalid)?;
    crate::native::validate_windows_relative_path(name.to_str().ok_or_else(invalid)?)?;
    let _ancestors = crate::native::files::pin_directory_path(parent)?;
    let policy = Policy::new()?;
    policy.verify(&open(parent)?, true, true)?;
    // DELETE, READ_CONTROL, SYNCHRONIZE and attributes; no payload write access.
    // Deny concurrent writers while permitting object-bound rename/delete.
    let file = OpenOptions::new()
        .access_mode(0x0013_0080)
        .share_mode(5)
        .security_qos_flags(0x0001_0000)
        .custom_flags(0x0220_0000)
        .open(absolute)?;
    policy.verify(&file, directory, false)?;
    Ok(file)
}

pub(super) fn rename_handle_no_replace(
    source: &File,
    parent: &File,
    name: &std::ffi::OsStr,
    directory: bool,
) -> io::Result<()> {
    crate::native::validate_windows_relative_path(name.to_str().ok_or_else(invalid)?)?;
    let policy = Policy::new()?;
    policy.verify(source, directory, false)?;
    policy.verify(parent, true, true)?;
    private_fs::rename_by_handle_no_replace(source, parent, name, directory).map_err(|_| invalid())
}

pub(super) fn delete_handle(source: &File, directory: bool) -> io::Result<()> {
    Policy::new()?.verify(source, directory, false)?;
    private_fs::delete_by_handle(source, directory).map_err(|_| invalid())
}

pub(super) fn create(path: &Path) -> io::Result<()> {
    let absolute = std::path::absolute(path)?;
    let path = absolute.as_path();
    if !path.exists() {
        crate::native::create_private_dir_all(path)?;
    }
    let _ancestors = crate::native::files::pin_directory_path(path)?;
    // Acquisition proves local ACL-capable storage and rejects every ancestor
    // reparse point. An existing media ACL differs only at the final directory.
    match private_fs::PrivateDir::open(path) {
        Ok(_) | Err(private_fs::Error::InsecureAcl) => {}
        Err(_) => return Err(invalid()),
    }
    let policy = Policy::new()?;
    let mut root = open(path)?;
    policy.verify(&root, true, true)?;
    walk(path, |entry, directory| {
        policy.verify(&open(entry)?, directory, false)
    })?;
    // Both the original and replacement ACL grant only this process user access.
    // Existing unsafe trees have already been rejected; no contents are written
    // during this transition from a strict to an inheritable private directory.
    policy.set(&mut root, true)?;
    // Elevated tokens can default new objects to a group owner even when the
    // inherited DACL contains only this user. Refuse that profile before a tool
    // creates media: group owners have implicit permission to change the DACL.
    let probe = tempfile::Builder::new()
        .prefix(".drmr-owner-probe-")
        .tempfile_in(path)?;
    let descriptor = GetSecurityInfo(
        probe.as_file(),
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Owner,
    )?;
    if GetSecurityDescriptorOwner(&descriptor)? != Some(&*policy.user) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "media conversion requires a regular Windows terminal; close the administrator terminal and try again",
        ));
    }
    Ok(())
}

pub(super) fn seal(path: &Path) -> io::Result<()> {
    let absolute = std::path::absolute(path)?;
    let path = absolute.as_path();
    let _ancestors = crate::native::files::pin_directory_path(path)?;
    match private_fs::PrivateDir::open(path) {
        Ok(_) | Err(private_fs::Error::InsecureAcl) => {}
        Err(_) => return Err(invalid()),
    }
    let policy = Policy::new()?;
    let root = open(path)?;
    policy.verify(&root, true, true)?;
    drop(root);
    walk(path, |entry, directory| {
        policy.verify(&open(entry)?, directory, false)
    })?;
    walk(path, |entry, directory| {
        let mut file = open(entry)?;
        policy.verify(&file, directory, false)?;
        if directory {
            let descriptor = GetSecurityInfo(
                &file,
                SeObjectType::SE_FILE_OBJECT,
                SecurityInformation::Dacl,
            )?;
            if ace_text(&descriptor)?.1 == policy.strict_ace {
                return Ok(());
            }
        }
        policy.set(&mut file, directory)
    })
}

// The empty new file inherits this verified owner's ACE from birth. The retained
// parent denies rename/delete sharing, and the file's ACL is protected before a
// caller can write any payload. This policy is separate from strict secret-file
// acquisition: inherited media directories never become valid secret parents.
pub(super) fn create_new(path: &Path) -> io::Result<File> {
    open_file(path, true)
}

pub(super) fn open_existing(path: &Path) -> io::Result<File> {
    open_file(path, false)
}

pub(super) fn create_or_truncate(path: &Path) -> io::Result<File> {
    let absolute = std::path::absolute(path)?;
    let parent = absolute.parent().ok_or_else(invalid)?;
    let _ancestors = match crate::native::files::pin_directory_path(parent) {
        Ok(held) => held,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            crate::native::create_private_dir_all(parent)?;
            crate::native::files::pin_directory_path(parent)?
        }
        Err(error) => return Err(error),
    };
    match private_fs::PrivateDir::open(parent) {
        Ok(_) => return crate::native::create_private_or_truncate(path),
        Err(private_fs::Error::InsecureAcl) => {
            // Fixed-output writers validate their own held target below. An
            // already prepared parent can contain concurrent sibling outputs;
            // do not walk or mutate those unrelated files while they are open.
            let policy = Policy::new()?;
            let directory = open(parent)?;
            policy.verify(&directory, true, true)?;
            let descriptor = GetSecurityInfo(
                &directory,
                SeObjectType::SE_FILE_OBJECT,
                SecurityInformation::Dacl,
            )?;
            if ace_text(&descriptor)?.1 != policy.media_ace {
                return Err(invalid());
            }
        }
        Err(private_fs::Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            crate::native::create_private_dir_all(parent)?;
            return crate::native::create_private_or_truncate(path);
        }
        Err(_) => return Err(invalid()),
    }
    match create_new(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let file = open_file(path, false)?;
            file.set_len(0)?;
            Ok(file)
        }
        result => result,
    }
}

fn open_file(path: &Path, create: bool) -> io::Result<File> {
    open_file_after_validation(path, create, || {})
}

fn open_file_after_validation(
    path: &Path,
    create: bool,
    after_validation: impl FnOnce(),
) -> io::Result<File> {
    let absolute = std::path::absolute(path)?;
    let path = absolute.as_path();
    let parent = path.parent().ok_or_else(invalid)?;
    let name = path.file_name().ok_or_else(invalid)?;
    crate::native::validate_windows_relative_path(name.to_str().ok_or_else(invalid)?)?;
    // A held immediate parent does not establish the identity of its higher
    // ancestors. Pin the entire local path before checking ACLs or opening a
    // writable leaf; keep those pins until the verified file handle is acquired.
    let _ancestors = crate::native::files::pin_directory_path(parent)?;
    match private_fs::PrivateDir::open(parent) {
        Ok(_) | Err(private_fs::Error::InsecureAcl) => {}
        Err(_) => return Err(invalid()),
    }
    let policy = Policy::new()?;
    let parent_handle = open(parent)?;
    policy.verify(&parent_handle, true, true)?;
    let descriptor = GetSecurityInfo(
        &parent_handle,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl,
    )?;
    if ace_text(&descriptor)?.1 != policy.media_ace {
        return Err(invalid());
    }
    after_validation();
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .access_mode(0xc006_0080)
        .share_mode(1)
        .security_qos_flags(0x0001_0000)
        .custom_flags(0x0020_0000)
        .create_new(create)
        .open(path)?;
    policy.verify(&file, false, false)?;
    policy.set(&mut file, false)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_media_child_creation_preserves_guards_and_existing_entries() {
        use std::io::Write as _;
        let root = crate::native::private_tempdir().unwrap();
        let parent = root.path().join("media");
        create(&parent).unwrap();
        let held_parent = pin(&parent).unwrap();
        let child_path = parent.join("video");
        let child = create_dir_new(root.path(), &child_path).unwrap();
        let identity = crate::native::FileIdentity::read(&child).unwrap();
        assert!(std::fs::rename(&parent, root.path().join("moved")).is_err());
        assert!(std::fs::rename(&child_path, parent.join("replaced")).is_err());
        let media = child_path.join("segment.m4s");
        let mut writer = create_new(&media).unwrap();
        writer.write_all(b"owner-only media").unwrap();
        private_fs::verify(&writer).unwrap();
        drop(writer);
        let failure = create_dir_new(root.path(), &child_path).unwrap_err();
        assert_eq!(failure.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&media).unwrap(), b"owner-only media");
        assert!(crate::native::create_private_new(&child_path.join("secret")).is_err());
        assert!(!child_path.join("secret").exists());
        assert!(
            crate::native::FileIdentity::read(&open(&child_path).unwrap()).unwrap() == identity
        );
        replacement_after_media_child_creation_is_never_adopted(root.path(), &parent);
        replacement_after_media_child_validation_is_never_moved(root.path(), &parent);
        native_mutation_names_and_occupied_targets_are_refused(root.path());
        drop(child);
        drop(held_parent);
        std::fs::rename(&parent, root.path().join("moved")).unwrap();
    }

    fn replacement_after_media_child_creation_is_never_adopted(scratch: &Path, parent: &Path) {
        use std::io::Write as _;
        let target = parent.join("raced-child");
        let original = scratch.join("retained-original");
        let mut replaced = std::path::PathBuf::new();
        let result = create_dir_new_after_creation(scratch, &target, |pending| {
            std::fs::rename(pending, &original).unwrap();
            crate::native::create_private_dir_new(pending).unwrap();
            crate::native::create_private_new(&pending.join("foreign.txt"))
                .unwrap()
                .write_all(b"foreign sentinel")
                .unwrap();
            replaced = pending.to_path_buf();
        });
        assert!(result.is_err());
        assert!(!target.exists());
        assert_eq!(
            std::fs::read(replaced.join("foreign.txt")).unwrap(),
            b"foreign sentinel"
        );
        assert!(
            private_fs::PrivateDir::open(&replaced).is_ok(),
            "foreign ACL changed"
        );
        assert!(original.is_dir());
    }

    fn replacement_after_media_child_validation_is_never_moved(scratch: &Path, parent: &Path) {
        use std::io::Write as _;
        let target = parent.join("late-raced-child");
        let original = scratch.join("late-retained-original");
        let mut replacement = std::path::PathBuf::new();
        let mut foreign_identity = None;
        let mut created_identity = None;
        let child = create_dir_new_with_hooks(
            scratch,
            &target,
            |_| {},
            |pending| {
                let before = OpenOptions::new()
                    .access_mode(0x80)
                    .share_mode(7)
                    .custom_flags(0x0220_0000)
                    .open(pending)
                    .unwrap();
                created_identity = Some(crate::native::FileIdentity::read(&before).unwrap());
                drop(before);
                std::fs::rename(pending, &original).unwrap();
                crate::native::create_private_dir_new(pending).unwrap();
                crate::native::create_private_new(&pending.join("foreign.txt"))
                    .unwrap()
                    .write_all(b"late foreign sentinel")
                    .unwrap();
                foreign_identity =
                    Some(crate::native::FileIdentity::read(&open(pending).unwrap()).unwrap());
                replacement = pending.to_path_buf();
            },
        )
        .unwrap();
        assert!(!original.exists());
        assert!(
            Some(crate::native::FileIdentity::read(&child).unwrap()) == created_identity,
            "the created directory identity changed"
        );
        assert!(target.is_dir());
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
        assert_eq!(
            std::fs::read(replacement.join("foreign.txt")).unwrap(),
            b"late foreign sentinel"
        );
        assert!(
            Some(crate::native::FileIdentity::read(&open(&replacement).unwrap()).unwrap())
                == foreign_identity,
            "the foreign directory identity changed"
        );
        assert!(
            private_fs::PrivateDir::open(&replacement).is_ok(),
            "foreign ACL changed"
        );
        drop(child);
    }

    fn native_mutation_names_and_occupied_targets_are_refused(root: &Path) {
        use std::io::Write as _;
        let parent = private_fs::PrivateDir::open(root).unwrap();
        let source = parent.create_dir("bound-directory").unwrap();
        let destination = parent.create_dir("occupied-directory").unwrap();
        destination
            .create("foreign.txt")
            .unwrap()
            .write_all(b"occupied foreign sentinel")
            .unwrap();
        for name in ["", ".", "..", "a/b", "a\\b", "a/", "a:"] {
            assert!(matches!(
                private_fs::rename_by_handle_no_replace(
                    &source,
                    &parent,
                    std::ffi::OsStr::new(name),
                    true
                ),
                Err(private_fs::Error::InvalidRelativePath)
            ));
        }
        assert!(matches!(
            private_fs::rename_by_handle_no_replace(
                &source,
                &parent,
                std::ffi::OsStr::new(&"x".repeat(256)),
                true
            ),
            Err(private_fs::Error::InvalidRelativePath)
        ));
        assert!(
            private_fs::rename_by_handle_no_replace(
                &source,
                &parent,
                "occupied-directory".as_ref(),
                true
            )
            .is_err()
        );
        assert!(private_fs::delete_by_handle(&destination, true).is_err());
        assert_eq!(
            std::fs::read(root.join("occupied-directory/foreign.txt")).unwrap(),
            b"occupied foreign sentinel"
        );
        assert!(root.join("bound-directory").is_dir());
    }

    #[test]
    fn a_higher_media_ancestor_stays_pinned_through_writable_leaf_acquisition() {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let ancestor = temp.path().join("ancestor");
        let media = ancestor.join("nested").join("media");
        crate::native::create_private_dir_all(&media).unwrap();
        create(&media).unwrap();
        let target = media.join("output.mp4");
        create_new(&target).unwrap().write_all(b"original").unwrap();
        let moved = temp.path().join("moved");
        let mut attempted = false;
        let file = open_file_after_validation(&target, false, || {
            attempted = true;
            let renamed = std::fs::rename(&ancestor, &moved);
            // Restore an unexpectedly moved tree before asserting, so a failing
            // regression still leaves the original fixture available to inspect.
            if renamed.is_ok() {
                std::fs::rename(&moved, &ancestor).unwrap();
            }
            assert!(
                renamed.is_err(),
                "higher ancestor moved before writable leaf open"
            );
        })
        .unwrap();
        assert!(attempted);
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        file.set_len(0).unwrap();
        drop(file);
        std::fs::rename(&ancestor, &moved).unwrap();
        assert_eq!(
            std::fs::read(moved.join("nested/media/output.mp4")).unwrap(),
            b""
        );
    }

    #[test]
    fn a_held_media_parent_cannot_be_renamed_before_file_creation() {
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().join("private");
        crate::native::create_private_dir_all(&private).unwrap();
        let moved = temp.path().join("moved");
        let held = open(&private).unwrap();
        let rename = std::fs::rename(&private, &moved);
        drop(held);
        if rename.is_ok() {
            std::fs::rename(&moved, &private).unwrap();
        }
        assert!(
            rename.is_err(),
            "media parent moved while its guard was held"
        );
        std::fs::rename(&private, &moved).unwrap();
    }
}
