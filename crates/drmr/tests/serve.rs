//! `drmr serve` as a process: the console server inside the CLI, configured from the
//! `DRMR_*` environment, whose common operations run this same executable.

use serde_json::Value;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

struct Server {
    child: Child,
    address: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(input: &Path, output: &Path) -> Server {
    let mut child = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("serve")
        .env("DRMR_INPUT_ROOT", input)
        .env("DRMR_OUTPUT_ROOT", output)
        .env("DRMR_BIND_PORT", "0")
        .env_remove("DRMR_BIND_HOST")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut banner = String::new();
    stderr.read_line(&mut banner).unwrap();
    // The server writes a record per request to stderr: keep reading it, or a closed pipe
    // (or a full one) would stop the handlers.
    std::thread::spawn(move || {
        for line in stderr.lines().map_while(Result::ok) {
            drop(line);
        }
    });
    let address = banner
        .trim()
        .strip_prefix("* Running on http://")
        .unwrap_or_else(|| panic!("no banner: {banner:?}"))
        .to_string();
    Server { child, address }
}

#[cfg(unix)]
#[test]
fn an_unfinished_request_body_cannot_prevent_confirmed_signal_shutdown() {
    use rustix::process::{Pid, Signal, kill_process};
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    let mut server = start(&input, &dir.path().join("output"));
    let mut stalled = TcpStream::connect(&server.address).unwrap();
    stalled
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(stalled, "POST /api/common/sessions HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nExpect: 100-continue\r\n\r\n", server.address).unwrap();
    let mut interim = String::new();
    BufReader::new(stalled.try_clone().unwrap())
        .read_line(&mut interim)
        .unwrap();
    assert!(interim.starts_with("HTTP/1.1 100"), "{interim}");
    let pid = Pid::from_raw(i32::try_from(server.child.id()).unwrap()).unwrap();
    kill_process(pid, Signal::TERM).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "an unfinished body must not hold shutdown open"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(stalled);
}

/// One request; the status and the body of a `Content-Length` answer.
fn request(server: &Server, method: &str, path: &str, headers: &str, body: &str) -> (u16, Value) {
    let mut stream = TcpStream::connect(&server.address).unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n{headers}\
         Content-Length: {}\r\n\r\n{body}",
        server.address,
        body.len()
    )
    .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    let status = answer[9..12].parse().unwrap();
    let (_, payload) = answer.split_once("\r\n\r\n").unwrap();
    (status, serde_json::from_str(payload).unwrap_or(Value::Null))
}

#[test]
fn serve_exposes_the_common_api_with_a_same_origin_guard() {
    let dir = tempfile::tempdir().unwrap();
    let (input, output) = (dir.path().join("input"), dir.path().join("output"));
    std::fs::create_dir(&input).unwrap();
    let server = start(&input, &output);
    assert!(
        server.address.starts_with("127.0.0.1:"),
        "loopback by default"
    );
    let (status, capabilities) = request(&server, "GET", "/api/common/capabilities", "", "");
    assert_eq!(status, 200, "{capabilities}");
    assert_eq!(capabilities["schema_version"], 1);
    let foreign = "Content-Type: application/json\r\nOrigin: https://evil.test\r\n";
    let (status, refused) = request(&server, "POST", "/api/common/jobs", foreign, "{}");
    assert_eq!(status, 403, "{refused}");
    for path in ["/api/platform", "/api/convert", "/api/catalog", "/api/hls"] {
        assert_eq!(request(&server, "GET", path, "", "").0, 404, "{path}");
    }
}

#[test]
fn serve_refuses_to_start_without_its_roots() {
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("serve")
        .env_remove("DRMR_INPUT_ROOT")
        .env_remove("DRMR_OUTPUT_ROOT")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: DRMR_INPUT_ROOT and DRMR_OUTPUT_ROOT must both be set\n"
    );
}
