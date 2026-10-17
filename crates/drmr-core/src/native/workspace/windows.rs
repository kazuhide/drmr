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
    const READ_CONTROL_WRITE_DAC_READ_ATTRIBUTES: u32 = 0x0006_0080;
    const BACKUP_SEMANTICS_OPEN_REPARSE_POINT: u32 = 0x0220_0000;
    OpenOptions::new()
        .access_mode(READ_CONTROL_WRITE_DAC_READ_ATTRIBUTES)
        .share_mode(1)
        .security_qos_flags(0x0001_0000)
        .custom_flags(BACKUP_SEMANTICS_OPEN_REPARSE_POINT)
        .open(path)
}

pub(super) fn create(path: &Path) -> io::Result<()> {
    let absolute = std::path::absolute(path)?;
    let path = absolute.as_path();
    if !path.exists() {
        crate::native::create_private_dir_all(path)?;
    }
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
    let absolute = std::path::absolute(path)?;
    let path = absolute.as_path();
    let parent = path.parent().ok_or_else(invalid)?;
    let name = path.file_name().ok_or_else(invalid)?;
    crate::native::validate_windows_relative_path(name.to_str().ok_or_else(invalid)?)?;
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
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .access_mode(0xc006_0080)
        .share_mode(1)
        .security_qos_flags(0x0001_0000)
        .custom_flags(0x0020_0000)
        .create_new(true)
        .open(path)?;
    policy.verify(&file, false, false)?;
    policy.set(&mut file, false)?;
    Ok(file)
}
