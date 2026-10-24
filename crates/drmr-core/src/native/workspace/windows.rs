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
