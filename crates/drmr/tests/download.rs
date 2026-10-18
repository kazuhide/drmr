//! Remote input validation must finish before any network or media work starts.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread::JoinHandle;
use std::time::Duration;

struct Server {
    url: String,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "http://{}/input.m3u8?credential=private-sentinel",
            listener.local_addr().unwrap()
        );
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let counter = Arc::clone(&requests);
        let stopping = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut socket, _)) => {
                        counter.fetch_add(1, Ordering::SeqCst);
                        socket
                            .set_read_timeout(Some(Duration::from_millis(100)))
                            .unwrap();
                        socket
                            .set_write_timeout(Some(Duration::from_millis(100)))
                            .unwrap();
                        let _ = socket.read(&mut [0; 4096]);
                        let body = b"#EXTM3U\n#EXT-X-ENDLIST\n";
                        let header = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = socket
                            .write_all(header.as_bytes())
                            .and_then(|()| socket.write_all(body));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("test listener failed: {error}"),
                }
            }
        });
        Self {
            url,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    fn assert_unused(&self) {
        assert_eq!(self.requests.load(Ordering::SeqCst), 0);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}

fn run(server: &Server, output: &Path, extra: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args([
            "convert",
            &server.url,
            "--to",
            "clear",
            "--format",
            "hls",
            "--output",
        ])
        .arg(output)
        .args(extra)
        .output()
        .unwrap()
}

fn refused(result: &Output, token: &str) {
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(result.stdout, [] as [u8; 0]);
    let diagnostic = String::from_utf8_lossy(&result.stderr);
    assert!(diagnostic.contains(token), "{diagnostic}");
    assert!(!diagnostic.contains("private-sentinel"));
}

fn policy(path: &Path, contents: &str) {
    drmr_core::native::create_private_or_truncate(path)
        .unwrap()
        .write_all(contents.as_bytes())
        .unwrap();
}

#[test]
fn url_requires_a_download_directory_before_any_request() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let server = Server::new();
    let output = root.path().join("output");
    let result = run(&server, &output, &[]);
    refused(&result, "--download-dir");
    server.assert_unused();
    assert!(!output.exists());
}

#[test]
fn overlapping_or_existing_destinations_are_refused_before_acquisition() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let server = Server::new();
    let download = root.path().join("source");
    let result = run(
        &server,
        &download,
        &["--download-dir".as_ref(), download.as_os_str()],
    );
    refused(&result, "download-output-overlap");
    let result = run(
        &server,
        &download.join("result"),
        &["--download-dir".as_ref(), download.as_os_str()],
    );
    refused(&result, "download-destination-unsafe");
    let output = root.path().join("result");
    let nested = output.join("source");
    let result = run(
        &server,
        &output,
        &["--download-dir".as_ref(), nested.as_os_str()],
    );
    refused(&result, "download-destination-unsafe");
    drmr_core::native::create_private_dir_all(&download).unwrap();
    std::fs::write(download.join("keep"), b"original").unwrap();
    let result = run(
        &server,
        &output,
        &["--download-dir".as_ref(), download.as_os_str()],
    );
    refused(&result, "download-directory-exists");
    assert_eq!(std::fs::read(download.join("keep")).unwrap(), b"original");
    let other_download = root.path().join("other-source");
    std::fs::write(&output, b"existing output").unwrap();
    let result = run(
        &server,
        &output,
        &["--download-dir".as_ref(), other_download.as_os_str()],
    );
    refused(&result, "output-exists-or-unsafe");
    assert_eq!(std::fs::read(&output).unwrap(), b"existing output");
    assert!(!other_download.exists());
    server.assert_unused();
}

#[cfg(unix)]
#[test]
fn destination_parents_follow_native_symlink_then_parent_semantics() {
    use std::os::unix::fs::symlink;
    let root = drmr_core::native::private_tempdir().unwrap();
    let server = Server::new();
    let nested = root.path().join("actual/nested");
    std::fs::create_dir_all(&nested).unwrap();
    symlink(&nested, root.path().join("link")).unwrap();
    let download = root.path().join("link/../source");
    let output = root.path().join("actual/source");
    let result = run(
        &server,
        &output,
        &["--download-dir".as_ref(), download.as_os_str()],
    );
    refused(&result, "download-output-overlap");
    server.assert_unused();
    assert!(!output.exists());
}

#[test]
fn fetch_policy_is_strict_private_configuration_and_never_echoes_values() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let server = Server::new();
    let output = root.path().join("output");
    let download = root.path().join("source");
    let config = root.path().join("policy.json");
    for contents in [
        "[]",
        "[[],{},true,true]",
        r#"{"allow_http":"private-sentinel"}"#,
        r#"{"allow_http":true,"allow_http":false}"#,
        r#"{"allowed_origins":"private-sentinel"}"#,
        r#"{"unknown":"private-sentinel"}"#,
        r#"{"max_total_bytes":null,"connections":null,"timeout":null,"attempts":null}"#,
        r#"{"headers_by_origin":{"x":{"Authorization":"one","Authorization":"private-sentinel"}}}"#,
    ] {
        policy(&config, contents);
        let result = run(
            &server,
            &output,
            &[
                "--download-dir".as_ref(),
                download.as_os_str(),
                "--fetch-policy".as_ref(),
                config.as_os_str(),
            ],
        );
        refused(&result, "fetch-policy-invalid");
    }
    server.assert_unused();
    assert!(!download.exists() && !output.exists());
}

#[test]
fn local_inputs_do_not_silently_accept_download_options() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(["convert", "local.m3u8", "--to", "clear", "--output"])
        .arg(root.path().join("output"))
        .arg("--download-dir")
        .arg(root.path().join("source"))
        .output()
        .unwrap();
    refused(&result, "download-options-require-url");
}
