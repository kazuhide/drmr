//! Windows job cleanup must finish before pathname guards can be released.
#![cfg(windows)]

use drmr_core::native::{self, ManagedChild};
use std::fs::OpenOptions;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::os::windows::fs::OpenOptionsExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const COUNT: usize = 4;
const ROLE: &str = "DRMR_PROCESS_DRAIN_TEST_ROLE";
const DIRECTORY: &str = "DRMR_PROCESS_DRAIN_TEST_DIRECTORY";
const ADDRESS: &str = "DRMR_PROCESS_DRAIN_TEST_ADDRESS";

fn command(role: &str, directory: &Path, address: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "process_drain_helper", "--nocapture"])
        .env(ROLE, role)
        .env(DIRECTORY, directory)
        .env(ADDRESS, address)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn ready(condition: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    while !condition() {
        assert!(Instant::now() < end, "process fixture did not become ready");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn process_drain_helper() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    let directory = std::path::PathBuf::from(std::env::var_os(DIRECTORY).unwrap());
    let address = std::env::var(ADDRESS).unwrap();
    if let Some(index) = role.strip_prefix("descendant-") {
        let mut lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(directory.join(format!("{index}.lock")))
            .unwrap();
        lock.write_all(b"pending descendant I/O").unwrap();
        let mut stream = TcpStream::connect(address).unwrap();
        std::fs::write(
            directory.join(format!("{index}.ready")),
            std::process::id().to_string(),
        )
        .unwrap();
        // The test supervisor holds the other endpoint open without sending data.
        // Termination must cancel this read and release the exclusive file handle.
        let mut byte = [0];
        let _ = stream.read_exact(&mut byte);
        return;
    }
    for index in 0..COUNT {
        let child = command(&format!("descendant-{index}"), &directory, &address)
            .spawn()
            .unwrap();
        drop(child);
    }
    ready(|| directory.join("release-parent").exists());
}

fn assert_drained(mode: &str) {
    let workspace = native::private_tempdir().unwrap();
    let directory = workspace.path();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let mut child = ManagedChild::spawn(command("parent", directory, &address), false).unwrap();
    let mut peers = Vec::new();
    ready(|| {
        (0..COUNT).all(|index| {
            std::fs::read_to_string(directory.join(format!("{index}.ready")))
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .is_some()
        })
    });
    // Keep native process identities while all descendants are blocked in I/O.
    // A zero-time wait distinguishes job membership from completed termination.
    let processes: Vec<_> = (0..COUNT)
        .map(|index| {
            let pid = std::fs::read_to_string(directory.join(format!("{index}.ready")))
                .unwrap()
                .parse::<u32>()
                .unwrap();
            winsafe::HPROCESS::OpenProcess(winsafe::co::PROCESS::SYNCHRONIZE, false, pid).unwrap()
        })
        .collect();
    let assert_released = || {
        for process in &processes {
            let state = process.WaitForSingleObject(Some(0));
            eprintln!("descendant termination state after {mode}: {state:?}");
        }
        assert_handles_released(directory);
    };
    for _ in 0..COUNT {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    peers.push(stream);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < end, "missing pending-I/O peer");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept failed: {error}"),
            }
        }
    }
    match mode {
        "kill" => {
            child.kill().unwrap();
            assert_released();
            child.wait().unwrap();
            drop(child);
        }
        "wait" => {
            std::fs::write(directory.join("release-parent"), b"exit").unwrap();
            assert!(child.wait().unwrap().success());
            assert_released();
            drop(child);
        }
        "drop" => drop(child),
        _ => unreachable!(),
    }
    // No eventual polling here: cleanup has already returned. Every descendant's
    // exclusive handle and blocked I/O must have been released at this boundary.
    assert_released();
    for index in 0..COUNT {
        std::fs::remove_file(directory.join(format!("{index}.lock"))).unwrap();
    }
    workspace
        .close()
        .expect("job cleanup must precede private workspace removal");
    drop(peers);
}

fn assert_handles_released(directory: &Path) {
    for index in 0..COUNT {
        drop(
            OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(0)
                .open(directory.join(format!("{index}.lock")))
                .expect("job cleanup returned while a descendant still held its file"),
        );
    }
}

#[test]
fn kill_drains_descendant_handles_and_pending_io_before_returning() {
    assert_drained("kill");
}
#[test]
fn wait_drains_descendant_handles_and_pending_io_after_parent_exit() {
    assert_drained("wait");
}
#[test]
fn drop_drains_descendant_handles_and_pending_io_before_workspace_cleanup() {
    assert_drained("drop");
}
