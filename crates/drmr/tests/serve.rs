//! `drmr serve` as a process: the console server inside the CLI, configured from the
//! `DRMR_*` environment, whose common operations run this same executable.

use serde_json::Value;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

struct Server {
    child: Child,
    address: String,
    stderr: Option<JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.stderr.take() {
            let _ = reader.join();
        }
    }
}

fn text_progress(line: &str, activity: &str, event: &str) {
    assert!(
        !line.contains('\u{1b}') && !line.contains("scopes"),
        "{line}"
    );
    if event == "failed" {
        assert!(line.starts_with("Failed serve in "), "{line}");
        assert!(line.ends_with(" | server-stopped-with-error"), "{line}");
    } else {
        let label = if activity == "serving" {
            "Running web console"
        } else {
            "Starting web console"
        };
        assert!(line.starts_with(&format!("  {label}")), "{line}");
        assert_eq!(line.contains("| elapsed "), event != "history", "{line}");
    }
}

fn start(input: &Path, output: &Path, progress: Option<&str>) -> Server {
    let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
    command.arg("serve");
    if let Some(mode) = progress {
        command.args(["--progress", mode]);
    }
    // Own the child before reading or checking any of its startup output.
    let mut server = Server {
        child: command
            .env("DRMR_INPUT_ROOT", input)
            .env("DRMR_OUTPUT_ROOT", output)
            .env("DRMR_BIND_PORT", "0")
            .env_remove("DRMR_BIND_HOST")
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
        address: String::new(),
        stderr: None,
    };
    let stderr = server.child.stderr.take().unwrap();
    let (sent, received) = mpsc::sync_channel(1);
    // The server writes a record per request to stderr: keep reading it, or a closed pipe
    // (or a full one) would stop the handlers.
    server.stderr = Some(std::thread::spawn(move || {
        let mut startup = Some(sent);
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(sender) = &startup {
                let ready = line.trim().starts_with("* Running on http://");
                if sender.send(line).is_err() {
                    break;
                }
                if ready {
                    startup = None;
                }
            }
        }
    }));
    let deadline = Instant::now() + Duration::from_secs(5);
    let next = || {
        received
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("server startup output must arrive before the readiness deadline")
    };
    let banner = if progress == Some("off") {
        next()
    } else {
        text_progress(&next(), "server-startup", "activity");
        let mut serving = false;
        loop {
            let line = next();
            if line.trim().starts_with("* Running on http://") {
                assert!(serving, "readiness must follow the serving activity");
                break line;
            }
            if line.starts_with("  Running web console |") {
                assert!(!serving, "serving must be announced once");
                text_progress(&line, "serving", "activity");
                serving = true;
            } else {
                text_progress(
                    &line,
                    if serving { "serving" } else { "server-startup" },
                    if line.contains("| elapsed ") {
                        "heartbeat"
                    } else {
                        "history"
                    },
                );
            }
        }
    };
    server.address = banner
        .trim()
        .strip_prefix("* Running on http://")
        .unwrap_or_else(|| panic!("no banner: {banner:?}"))
        .to_string();
    server
}

#[cfg(unix)]
#[test]
fn an_unfinished_request_body_cannot_prevent_confirmed_signal_shutdown() {
    use rustix::process::{Pid, Signal, kill_process};
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    let mut server = start(&input, &dir.path().join("output"), Some("off"));
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
    for progress in [None, Some("text"), Some("off")] {
        let server = start(&input, &output, progress);
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
}

#[test]
fn serve_refuses_to_start_without_its_roots() {
    for progress in [None, Some("text"), Some("off")] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
        command.arg("serve");
        if let Some(mode) = progress {
            command.args(["--progress", mode]);
        }
        let output = command
            .env_remove("DRMR_INPUT_ROOT")
            .env_remove("DRMR_OUTPUT_ROOT")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(output.stdout, [] as [u8; 0]);
        let stderr = String::from_utf8(output.stderr).unwrap();
        let error = "error: DRMR_INPUT_ROOT and DRMR_OUTPUT_ROOT must both be set\n";
        if progress == Some("off") {
            assert_eq!(stderr, error);
        } else {
            let lines: Vec<_> = stderr.lines().collect();
            assert!(lines.len() >= 3, "{stderr}");
            text_progress(lines[0], "server-startup", "activity");
            let mut errors = 0;
            for line in &lines[1..lines.len() - 1] {
                if *line == error.trim_end() {
                    errors += 1;
                } else {
                    text_progress(
                        line,
                        "server-startup",
                        if line.contains("| elapsed ") {
                            "heartbeat"
                        } else {
                            "history"
                        },
                    );
                }
            }
            assert_eq!(errors, 1, "{stderr}");
            text_progress(lines.last().unwrap(), "server-startup", "failed");
        }
    }
}
