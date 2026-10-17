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
            format!("D:P(A;;FA;;;{})(A;;FR;;;WD)", original.owner().unwrap())
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
