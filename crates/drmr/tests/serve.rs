//! `drmr serve` as a process: the console server inside the CLI, configured from the
//! `DRMR_*` environment, whose jobs run this same executable. Synthetic key material
//! only.

use serde_json::Value;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "10101010101010101010101010101010";
const IV: &str = "20202020202020202020202020202020";

/// One catalog entry whose manifest the pipeline refuses at once (no `<BaseURL>`), so a
/// conversion job ends quickly whatever `drmr run` is wired to.
fn stage_entry(catalog: &Path, asset_id: &str) {
    let entry = catalog.join(asset_id);
    drmr_core::native::create_private_dir_all(&entry).unwrap();
    let dash = entry.join("download/manifest.example.test").join(asset_id);
    std::fs::create_dir_all(dash.join("dash")).unwrap();
    std::fs::create_dir_all(dash.join("hls")).unwrap();
    std::fs::write(
        dash.join("dash/manifest.mpd"),
        "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\"/>\n",
    )
    .unwrap();
    std::fs::write(dash.join("hls/manifest.m3u8"), "#EXTM3U\n").unwrap();
    drmr_core::native::create_private_new(&entry.join("keys.env"))
        .unwrap()
        .write_all(format!("DRM_KID={KID}\nDRM_KEY={KEY}\nDRM_IV={IV}\n").as_bytes())
        .unwrap();
    let descriptor = serde_json::json!({
        "asset_id": asset_id,
        "label": "Synthetic",
        "mpd_path": format!("download/manifest.example.test/{asset_id}/dash/manifest.mpd"),
        "hls_master_path": format!("download/manifest.example.test/{asset_id}/hls/manifest.m3u8"),
        "keys_file": "keys.env",
    });
    std::fs::write(entry.join("asset.json"), descriptor.to_string()).unwrap();
}

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

fn start(catalog: &Path, output: &Path) -> Server {
    let mut child = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("serve")
        .env("DRMR_CATALOG_ROOT", catalog)
        .env("DRMR_OUTPUT_ROOT", output)
        .env("DRMR_BIND_PORT", "0")
        .env_remove("DRMR_BIND_HOST")
        .env_remove("DRMR_HLS_PROFILES")
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut banner = String::new();
    stderr.read_line(&mut banner).unwrap();
    // The server writes a record per request to stderr: keep reading it, or a closed pipe
    // (or a full one) would stop the handlers.
    std::thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::sink()));
    let address = banner
        .trim()
        .strip_prefix("* Running on http://")
        .unwrap_or_else(|| panic!("no banner: {banner:?}"))
        .to_string();
    Server { child, address }
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
fn serve_answers_the_api_and_its_jobs_run_this_executable() {
    let dir = tempfile::tempdir().unwrap();
    let (catalog, output) = (dir.path().join("catalog"), dir.path().join("output"));
    std::fs::create_dir(&catalog).unwrap();
    stage_entry(&catalog, "synthetic-asset");
    let server = start(&catalog, &output);
    assert!(
        server.address.starts_with("127.0.0.1:"),
        "loopback by default"
    );

    // The jobs' CLI is this executable, which carries native batch workers.
    let (status, platform) = request(&server, "GET", "/api/platform", "", "");
    assert_eq!(status, 200);
    assert_eq!(platform["batch_available"], Value::Bool(true));
    assert_eq!(platform["concurrency"], 1);

    // A mutating route keeps the same-origin guard.
    let json = "Content-Type: application/json\r\n";
    let foreign = format!("{json}Origin: https://evil.test\r\n");
    let (status, refused) = request(&server, "POST", "/api/convert", &foreign, "{}");
    assert_eq!(
        (status, refused),
        (403, serde_json::json!({"error": "same-origin-required"}))
    );

    // A conversion launches `drmr run …` (this executable) and ends with its exit code; its
    // output is the job log.
    let body = r#"{"asset_id": "synthetic-asset"}"#;
    let (status, accepted) = request(&server, "POST", "/api/convert", json, body);
    assert_eq!(status, 202, "{accepted}");
    let job_id = accepted["job_id"].as_str().unwrap().to_string();
    let deadline = Instant::now() + Duration::from_secs(60);
    let finished = loop {
        let (_, job) = request(&server, "GET", &format!("/api/jobs/{job_id}"), "", "");
        if job["state"] != "running" {
            break job;
        }
        assert!(Instant::now() < deadline, "the job never ended: {job}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(finished["state"], "failed", "{finished}");
    assert!(finished["exit_code"].as_i64().is_some_and(|code| code != 0));
    let log = std::fs::read_to_string(output.join("_jobs").join(format!("{job_id}.jsonl")));
    let log = log.unwrap();
    assert!(!log.is_empty(), "the CLI's own output is the log");
    for secret in [KEY, IV] {
        assert!(!log.contains(secret), "no key material in the log");
    }
}

#[test]
fn serve_refuses_to_start_without_its_roots() {
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("serve")
        .env_remove("DRMR_CATALOG_ROOT")
        .env_remove("DRMR_OUTPUT_ROOT")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "error: DRMR_CATALOG_ROOT and DRMR_OUTPUT_ROOT must both be set\n"
    );
}
