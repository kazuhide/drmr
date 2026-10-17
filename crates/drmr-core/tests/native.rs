//! Native filesystem and child-process contracts, including Windows security boundaries.

use drmr_core::native;
use std::io::{Read, Write};

#[test]
fn native_resolution_keeps_missing_children_below_the_canonical_existing_parent() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("spaces and 日本語");
    std::fs::create_dir(&parent).unwrap();
    let resolved = native::resolve_path(&parent.join("missing/../future.mp4")).unwrap();
    assert_eq!(resolved, parent.canonicalize().unwrap().join("future.mp4"));
}

#[cfg(unix)]
#[test]
fn native_resolution_applies_parent_components_after_resolving_a_link() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("real/nested");
    std::fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&target, temp.path().join("link")).unwrap();
    assert_eq!(
        native::resolve_path(&temp.path().join("link/../future")).unwrap(),
        target
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join("future")
    );
}

#[cfg(unix)]
#[test]
fn private_reads_and_updates_reject_links_before_truncating_any_contents() {
    use std::os::unix::fs::symlink;
    for hard_link in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        native::create_private_new(&target)
            .unwrap()
            .write_all(b"unchanged")
            .unwrap();
        let alias = temp.path().join("alias");
        if hard_link {
            std::fs::hard_link(&target, &alias).unwrap();
        } else {
            symlink(&target, &alias).unwrap();
        }
        let outcomes = [
            native::open_private_read(&alias).is_err(),
            native::create_private_update(&alias).is_err(),
            native::create_private_or_truncate(&alias).is_err(),
        ];
        assert_eq!(std::fs::read(&target).unwrap(), b"unchanged");
        assert!(outcomes.into_iter().all(|rejected| rejected));
    }
}

#[cfg(unix)]
#[test]
fn private_fifo_inputs_are_rejected_without_waiting_for_a_writer() {
    use std::time::{Duration, Instant};
    let temp = tempfile::tempdir().unwrap();
    assert!(
        Command::new("/usr/bin/mkfifo")
            .args(["-m", "600"])
            .arg(temp.path().join("fifo"))
            .status()
            .unwrap()
            .success()
    );
    let mut child =
        native::ManagedChild::spawn(probe_command("private-fifo", temp.path()), true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "private FIFO admission blocked");
        std::thread::park_timeout(Duration::from_millis(10));
    }
}

#[test]
fn temporary_capture_files_are_private_and_support_cloned_handles() {
    use std::io::{Seek, SeekFrom};
    let mut capture = native::private_tempfile().unwrap();
    let mut writer = capture.try_clone().unwrap();
    writer.write_all(b"private output").unwrap();
    drop(writer);
    capture.seek(SeekFrom::Start(0)).unwrap();
    let mut contents = String::new();
    capture.read_to_string(&mut contents).unwrap();
    assert_eq!(contents, "private output");
    #[cfg(windows)]
    private_fs::verify(&capture).unwrap();
}

#[test]
fn private_replacement_validates_before_truncating() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("private");
    native::create_private_dir_all(&directory).unwrap();
    let path = directory.join("key.bin");
    native::create_private_new(&path)
        .unwrap()
        .write_all(b"old contents")
        .unwrap();
    native::create_private_or_truncate(&path)
        .unwrap()
        .write_all(b"new")
        .unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"new");
}

#[test]
fn private_directory_creation_is_exclusive() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("private");
    native::create_private_dir_all(&parent).unwrap();
    let child = parent.join("snapshot");
    native::create_private_dir_new(&child).unwrap();
    assert_eq!(
        native::create_private_dir_new(&child).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
}

#[test]
fn private_creation_is_exclusive_and_preserves_existing_contents() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("private");
    native::create_private_dir_all(&directory).unwrap();
    let path = directory.join("key.bin");
    native::create_private_new(&path)
        .unwrap()
        .write_all(b"synthetic fixture")
        .unwrap();
    assert_eq!(
        native::create_private_new(&path).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    let mut bytes = Vec::new();
    native::open_private_read(&path)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, b"synthetic fixture");
}

#[test]
fn concurrent_private_creation_admits_exactly_one_writer() {
    use std::sync::{Arc, Barrier};
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("private");
    native::create_private_dir_all(&directory).unwrap();
    let path = directory.join("exclusive.bin");
    let ready = Arc::new(Barrier::new(8));
    let winners = std::thread::scope(|scope| {
        let children: Vec<_> = (0..8u8)
            .map(|index| {
                let ready = Arc::clone(&ready);
                let path = &path;
                scope.spawn(move || {
                    ready.wait();
                    native::create_private_new(path)
                        .map(|mut file| {
                            file.write_all(&[index]).unwrap();
                            index
                        })
                        .ok()
                })
            })
            .collect();
        children
            .into_iter()
            .filter_map(|child| child.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(winners.len(), 1);
    assert_eq!(std::fs::read(path).unwrap(), winners);
}

#[cfg(windows)]
#[test]
fn private_creation_rejects_hard_links_and_stream_names() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("private");
    native::create_private_dir_all(&directory).unwrap();
    let path = directory.join("key.bin");
    native::create_private_new(&path)
        .unwrap()
        .write_all(b"unchanged")
        .unwrap();
    std::fs::hard_link(&path, directory.join("alias.bin")).unwrap();
    assert!(native::open_private_read(&path).is_err());
    assert!(native::create_private_new(&directory.join("key.bin:stream")).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"unchanged");
}

#[cfg(windows)]
#[test]
fn media_workspace_checks_default_ownership_before_generating_payloads() {
    use windows_permissions::constants::{SeObjectType, SecurityInformation};
    let temp = tempfile::tempdir().unwrap();
    let empty = temp.path().join("empty-probe");
    let file = std::fs::File::create(&empty).unwrap();
    let descriptor = windows_permissions::wrappers::GetSecurityInfo(
        &file,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Owner,
    )
    .unwrap();
    let user = windows_permissions::utilities::current_process_sid().unwrap();
    let workspace = temp.path().join("media");
    let result = native::create_media_dir_all(&workspace);
    if descriptor.owner() == Some(&*user) {
        result.unwrap();
    } else {
        let error = result.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(error.to_string().contains("regular Windows terminal"));
    }
    assert_eq!(std::fs::read_dir(&workspace).unwrap().count(), 0);
}

#[test]
fn media_file_creation_is_exclusive_and_strict_before_payload_writes() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("media");
    native::create_media_dir_all(&workspace).unwrap();
    let path = workspace.join("playlist.m3u8");
    let mut file = native::create_media_new(&path).unwrap();
    #[cfg(windows)]
    private_fs::verify(&file).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
    file.write_all(b"synthetic playlist").unwrap();
    drop(file);
    assert!(native::create_media_new(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"synthetic playlist");
    native::open_private_read(&path).unwrap();
}

#[test]
fn media_sealing_preserves_a_strict_secret_child_directory() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("media");
    native::create_media_dir_all(&workspace).unwrap();
    let keys = workspace.join("keys");
    native::create_private_dir_all(&keys).unwrap();
    native::create_private_new(&keys.join("key.bin"))
        .unwrap()
        .write_all(b"synthetic")
        .unwrap();
    std::fs::write(workspace.join("output.bin"), b"output").unwrap();
    native::seal_media_tree(&workspace).unwrap();
    native::open_private_read(&workspace.join("output.bin")).unwrap();
    native::create_private_new(&keys.join("next.bin")).unwrap();
    assert_eq!(std::fs::read(keys.join("key.bin")).unwrap(), b"synthetic");
}

#[cfg(windows)]
#[test]
fn unsafe_media_acls_are_refused_without_repairing_or_rewriting_any_entry() {
    use windows_permissions::constants::{SeObjectType, SecurityInformation};
    use windows_permissions::wrappers::{GetSecurityInfo, SetSecurityInfo};
    use windows_permissions::{LocalBox, SecurityDescriptor};
    for (directory, null_acl) in [(true, true), (true, false), (false, false)] {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("media");
        native::create_media_dir_all(&workspace).unwrap();
        let payload = workspace.join("output.bin");
        std::fs::write(&payload, b"unchanged").unwrap();
        // A parent ACL mutation can itself remove the child's inherited ACE.
        // Retain access before constructing that fixture, so unchanged bytes
        // remain independently testable even when a later reopen is denied.
        let mut payload_reader = std::fs::File::open(&payload).unwrap();
        let path = if directory { &workspace } else { &payload };
        let mut control = acl_control(path);
        let original = GetSecurityInfo(
            &control,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Owner | SecurityInformation::Dacl,
        )
        .unwrap();
        let replacement: Option<LocalBox<SecurityDescriptor>> = (!null_acl).then(|| {
            format!(
                "D:P(A;;FA;;;{})(A;;FR;;;WD)",
                windows_permissions::utilities::current_process_sid().unwrap()
            )
            .parse()
            .unwrap()
        });
        SetSecurityInfo(
            &mut control,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
            None,
            None,
            replacement.as_ref().and_then(|sd| sd.dacl()),
            None,
        )
        .unwrap();
        let before = descriptor_text(&control);
        let payload_before = descriptor_text(&payload_reader);
        assert!(native::create_media_dir_all(&workspace).is_err());
        assert!(native::seal_media_tree(&workspace).is_err());
        if directory {
            let rejected = workspace.join("must-not-exist");
            assert!(native::create_media_new(&rejected).is_err());
            assert!(!rejected.exists());
        }
        assert_eq!(descriptor_text(&control), before);
        assert_eq!(descriptor_text(&payload_reader), payload_before);
        let mut contents = Vec::new();
        payload_reader.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"unchanged");
        SetSecurityInfo(
            &mut control,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
            None,
            None,
            original.dacl(),
            None,
        )
        .unwrap();
    }
}

#[cfg(windows)]
fn acl_control(path: &Path) -> std::fs::File {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .access_mode(0x0006_0080)
        .custom_flags(0x0220_0000)
        .open(path)
        .unwrap()
}

#[cfg(windows)]
fn descriptor_text(file: &std::fs::File) -> std::ffi::OsString {
    use windows_permissions::constants::{SeObjectType, SecurityInformation};
    let descriptor = windows_permissions::wrappers::GetSecurityInfo(
        file,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Owner | SecurityInformation::Dacl,
    )
    .unwrap();
    windows_permissions::wrappers::ConvertSecurityDescriptorToStringSecurityDescriptor(
        &descriptor,
        SecurityInformation::Owner | SecurityInformation::Dacl,
    )
    .unwrap()
}

#[cfg(windows)]
#[test]
fn media_workspaces_refuse_junctions_and_hard_links_before_sealing() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("media");
    native::create_media_dir_all(&workspace).unwrap();
    let outside = temp.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("marker"), b"unchanged").unwrap();
    let junction = workspace.join("junction");
    let result = Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&outside)
        .output()
        .unwrap();
    assert!(result.status.success());
    assert!(native::create_media_dir_all(&junction).is_err());
    assert!(native::seal_media_tree(&workspace).is_err());
    assert_eq!(std::fs::read(outside.join("marker")).unwrap(), b"unchanged");
    std::fs::remove_dir(&junction).unwrap();
    let target = workspace.join("original");
    std::fs::write(&target, b"unchanged").unwrap();
    std::fs::hard_link(&target, workspace.join("alias")).unwrap();
    assert!(native::seal_media_tree(&workspace).is_err());
    assert_eq!(std::fs::read(target).unwrap(), b"unchanged");
}

#[cfg(windows)]
#[test]
fn external_media_files_are_owner_only_from_creation_and_seal_to_strict_acls() {
    use windows_permissions::constants::{AccessRights, SeObjectType, SecurityInformation};
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("media");
    native::create_media_dir_all(&workspace).unwrap();
    let mut child =
        native::ManagedChild::spawn(probe_command("media-files", &workspace), true).unwrap();
    assert!(child.wait().unwrap().success());
    drop(child);
    for path in [
        workspace.join("output.bin"),
        workspace.join("nested/output.bin"),
    ] {
        let file = std::fs::File::open(&path).unwrap();
        let sd = windows_permissions::wrappers::GetSecurityInfo(
            &file,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Owner | SecurityInformation::Dacl,
        )
        .unwrap();
        let acl = sd.dacl().unwrap();
        assert_eq!(acl.len(), 1);
        assert_eq!(acl.get_ace(0).unwrap().sid(), sd.owner());
        assert_eq!(acl.get_ace(0).unwrap().mask(), AccessRights::FileAllAccess);
        assert!(
            native::open_private_read(&path).is_err(),
            "inherited ACL is not a strict secret ACL"
        );
    }
    native::seal_media_tree(&workspace).unwrap();
    for path in [
        workspace.join("output.bin"),
        workspace.join("nested/output.bin"),
    ] {
        private_fs::verify(&native::open_private_read(&path).unwrap()).unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"media fixture");
    }
    native::create_media_dir_all(&workspace).unwrap();
}

#[cfg(windows)]
#[test]
fn private_directory_acquisition_rejects_junction_ancestors() {
    let temp = tempfile::tempdir().unwrap();
    let private = temp.path().join("private");
    native::create_private_dir_all(&private).unwrap();
    let link = temp.path().join("junction");
    // Directory junctions need no symlink privilege. This Windows builtin is
    // used only to construct an adversarial fixture; production never uses a shell.
    let output = Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&link)
        .arg(&private)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "junction fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(native::create_private_dir_all(&link.join("child")).is_err());
    assert!(!private.join("child").exists());
}

#[cfg(windows)]
#[test]
fn inherited_access_is_rejected_without_hardening_or_truncation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("ordinary.json");
    std::fs::write(&path, b"unchanged").unwrap();
    assert!(native::open_private_read(&path).is_err());
    assert!(native::create_private_new(&path).is_err());
    assert!(native::create_private_or_truncate(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
}

#[cfg(windows)]
#[test]
fn device_and_network_namespaces_are_not_private_file_inputs() {
    for path in [r"\\.\pipe\drmr-private-probe", r"\\server\share\key.json"] {
        assert_eq!(
            native::open_private_read(Path::new(path))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }
}

#[cfg(windows)]
#[test]
fn private_files_have_a_protected_current_user_only_acl() {
    use windows_permissions::constants::{AccessRights, SeObjectType, SecurityInformation};
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("private");
    native::create_private_dir_all(&directory).unwrap();
    let path = directory.join("key.bin");
    let file = native::create_private_new(&path).unwrap();
    private_fs::verify(&file).unwrap();
    // Inspect the real descriptor using an independent API, rather than checking
    // only the same library that created the file.
    let descriptor = windows_permissions::wrappers::GetSecurityInfo(
        &file,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Owner | SecurityInformation::Dacl,
    )
    .unwrap();
    let acl = descriptor.dacl().unwrap();
    assert_eq!(acl.len(), 1);
    let entry = acl.get_ace(0).unwrap();
    assert_eq!(entry.sid(), descriptor.owner());
    assert_eq!(entry.mask(), AccessRights::FileAllAccess);
    drop(file);
    private_fs::verify(&native::open_private_read(&path).unwrap()).unwrap();
}

#[cfg(windows)]
#[test]
fn null_acl_and_an_extra_reader_are_rejected_without_repair() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_permissions::constants::{SeObjectType, SecurityInformation};
    use windows_permissions::{LocalBox, SecurityDescriptor};
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("private");
    native::create_private_dir_all(&directory).unwrap();
    for extra_reader in [false, true] {
        let path = directory.join(if extra_reader {
            "extra.bin"
        } else {
            "null.bin"
        });
        native::create_private_new(&path)
            .unwrap()
            .write_all(b"unchanged")
            .unwrap();
        let mut control = std::fs::OpenOptions::new()
            .access_mode(0x0006_0000)
            .open(&path)
            .unwrap();
        let original = windows_permissions::wrappers::GetSecurityInfo(
            &control,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Owner | SecurityInformation::Dacl,
        )
        .unwrap();
        let replacement: Option<LocalBox<SecurityDescriptor>> = extra_reader.then(|| {
            format!(
                "D:P(A;;FA;;;{})(A;;FR;;;WD)",
                windows_permissions::utilities::current_process_sid().unwrap()
            )
            .parse()
            .unwrap()
        });
        windows_permissions::wrappers::SetSecurityInfo(
            &mut control,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
            None,
            None,
            replacement.as_ref().and_then(|sd| sd.dacl()),
            None,
        )
        .unwrap();
        assert!(native::open_private_read(&path).is_err());
        assert!(native::create_private_or_truncate(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
        // Restore through the same handle so the temporary directory can clean up.
        windows_permissions::wrappers::SetSecurityInfo(
            &mut control,
            SeObjectType::SE_FILE_OBJECT,
            SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
            None,
            None,
            original.dacl(),
            None,
        )
        .unwrap();
    }
}

#[test]
fn managed_child_preserves_pipes_and_exit_status() {
    use std::process::Stdio;
    let mut command = probe_command("echo", Path::new("unused"));
    command.stdout(Stdio::piped());
    let mut child = native::ManagedChild::spawn(command, true).unwrap();
    let mut output = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert!(child.wait().unwrap().success());
    assert!(output.contains("drmr-native-probe: quotes ' \" and 日本語"));
}

use std::path::Path;
use std::process::{Command, Stdio};
#[cfg(windows)]
use std::time::{Duration, Instant};

fn probe_command(mode: &str, directory: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "child_probe", "--nocapture"])
        .env("DRMR_TEST_NATIVE_MODE", mode)
        .env("DRMR_TEST_NATIVE_DIRECTORY", directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[cfg(windows)]
fn await_condition(description: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "deadline waiting for {description}"
        );
        std::thread::park_timeout(Duration::from_millis(10));
    }
}

/// Reentered in an isolated test process. The ordinary suite invocation is a no-op.
#[test]
fn child_probe() {
    let Ok(mode) = std::env::var("DRMR_TEST_NATIVE_MODE") else {
        return;
    };
    if mode == "echo" {
        println!("drmr-native-probe: quotes ' \" and 日本語");
    } else {
        #[cfg(windows)]
        if mode == "media-files" {
            let directory = std::env::var_os("DRMR_TEST_NATIVE_DIRECTORY").unwrap();
            let directory = Path::new(&directory);
            std::fs::write(directory.join("output.bin"), b"media fixture").unwrap();
            std::fs::create_dir(directory.join("nested")).unwrap();
            std::fs::write(directory.join("nested/output.bin"), b"media fixture").unwrap();
            return;
        }
        #[cfg(unix)]
        if mode == "private-fifo" {
            let directory = std::env::var_os("DRMR_TEST_NATIVE_DIRECTORY").unwrap();
            let path = Path::new(&directory).join("fifo");
            assert!(native::open_private_read(&path).is_err());
            assert!(native::create_private_update(&path).is_err());
            assert!(native::create_private_or_truncate(&path).is_err());
        }
        #[cfg(windows)]
        windows_probe(
            &mode,
            Path::new(&std::env::var_os("DRMR_TEST_NATIVE_DIRECTORY").unwrap()),
        );
    }
}

#[cfg(windows)]
fn windows_probe(mode: &str, directory: &Path) {
    use std::os::windows::fs::OpenOptionsExt;
    if mode == "supervisor" || mode == "registration-gap" {
        let _managed;
        let _unregistered;
        if mode == "supervisor" {
            _managed =
                Some(native::ManagedChild::spawn(probe_command("tree", directory), true).unwrap());
            _unregistered = None;
        } else {
            // Establish the process-lifetime job, then model a child created
            // before its per-child job registration. It still inherits the
            // process job atomically at OS process creation.
            native::ManagedChild::spawn(probe_command("echo", directory), true)
                .unwrap()
                .wait()
                .unwrap();
            _managed = None;
            _unregistered = Some(probe_command("tree", directory).spawn().unwrap());
        }
        await_condition("grandchild ready", || {
            directory.join("grandchild.lock").exists()
        });
        std::fs::write(directory.join("supervisor.ready"), b"ready").unwrap();
        loop {
            std::thread::park_timeout(Duration::from_secs(1));
        }
    }
    let _grandchild = (mode == "tree").then(|| probe_command("hold", directory).spawn().unwrap());
    let name = if mode == "tree" {
        "child.lock"
    } else {
        "grandchild.lock"
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(directory.join(name))
        .unwrap();
    file.write_all(b"ready").unwrap();
    // Retain this exclusive handle until process exit: successful reacquisition
    // by the test proves this fixture has closed its handle or terminated.
    loop {
        std::thread::park_timeout(Duration::from_secs(1));
    }
}

#[cfg(windows)]
fn tree_is_ready(directory: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    ["child.lock", "grandchild.lock"].iter().all(|name| {
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(directory.join(name))
            .is_err_and(|e| e.raw_os_error() == Some(32))
    })
}

#[cfg(windows)]
fn tree_is_gone(directory: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    ["child.lock", "grandchild.lock"].iter().all(|name| {
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(directory.join(name))
            .is_ok()
    })
}

#[cfg(windows)]
#[test]
fn dropping_a_managed_child_terminates_the_child_and_grandchild() {
    let temp = tempfile::tempdir().unwrap();
    let child = native::ManagedChild::spawn(probe_command("tree", temp.path()), true).unwrap();
    await_condition("child tree ready", || tree_is_ready(temp.path()));
    drop(child);
    await_condition("child tree cleanup", || tree_is_gone(temp.path()));
}

#[cfg(windows)]
#[test]
fn supervisor_crash_closes_its_job_and_terminates_descendants() {
    assert_crash_cleanup("supervisor");
}

#[cfg(windows)]
#[test]
fn supervisor_crash_before_per_child_registration_still_terminates_descendants() {
    assert_crash_cleanup("registration-gap");
}

#[cfg(windows)]
fn assert_crash_cleanup(mode: &str) {
    let temp = tempfile::tempdir().unwrap();
    let mut supervisor = probe_command(mode, temp.path()).spawn().unwrap();
    // Keep cleanup even if a readiness assertion fails; no retry hides a missing
    // job assignment or a child that escaped while it was being registered.
    let outcome = std::panic::catch_unwind(|| {
        await_condition("supervisor ready", || {
            temp.path().join("supervisor.ready").exists() && tree_is_ready(temp.path())
        });
    });
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    outcome.unwrap();
    await_condition("crashed supervisor descendants", || {
        tree_is_gone(temp.path())
    });
}

#[test]
fn mutable_private_logs_can_be_read_while_the_writer_is_open() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("private");
    native::create_private_dir_all(&directory).unwrap();
    let path = directory.join("active.jsonl");
    let mut writer = native::create_private_new(&path).unwrap();
    writer.write_all(b"first\n").unwrap();
    let mut reader = native::open_private_shared_read(&path).unwrap();
    let mut text = String::new();
    reader.read_to_string(&mut text).unwrap();
    assert_eq!(text, "first\n");
    writer.write_all(b"second\n").unwrap();
    text.clear();
    reader.read_to_string(&mut text).unwrap();
    assert_eq!(text, "second\n");
}

#[cfg(windows)]
#[test]
fn mutable_log_reads_still_reject_inherited_access() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("ordinary.jsonl");
    std::fs::write(&path, b"unchanged").unwrap();
    assert!(native::open_private_shared_read(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"unchanged");
}
