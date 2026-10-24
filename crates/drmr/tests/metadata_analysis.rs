//! Metadata-only analysis stays key-free and separate from complete source admission.
use std::{
    path::Path,
    process::{Command, Output},
};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
    command.env("PATH", "");
    command
}

fn run(args: &[&str]) -> Output {
    command().args(args).output().unwrap()
}

fn report(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).expect("one JSON result")
}

fn no_secrets(output: &Output) {
    for bytes in [&output.stdout, &output.stderr] {
        let shown = String::from_utf8_lossy(bytes);
        for sentinel in [
            "metadata-query-sentinel",
            "metadata-key-sentinel",
            "metadata-auth-sentinel",
        ] {
            assert!(!shown.contains(sentinel), "sensitive input was displayed");
        }
    }
}

fn hls(path: &Path) {
    std::fs::write(path, "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"https://example.test/metadata-key-sentinel\"\n#EXTINF:2,\nnot-read.ts\n#EXT-X-ENDLIST\n").unwrap();
}

#[test]
fn metadata_local_formats_keep_the_existing_key_free_schema_without_tools() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let playlist = root.path().join("source.m3u8");
    hls(&playlist);
    let manifest = root.path().join("source.mpd");
    std::fs::write(&manifest, "<MPD><Period><AdaptationSet contentType=\"video\"><ContentProtection schemeIdUri=\"urn:mpeg:dash:mp4protection:2011\" value=\"cenc\"/><Representation id=\"video\" codecs=\"avc1.64001f\"/></AdaptationSet></Period></MPD>").unwrap();
    let mp4 = root.path().join("source.mp4");
    std::fs::write(&mp4, include_bytes!("fixtures/bbb/cenc-iv8.mp4")).unwrap();
    for (path, format, evidence, protection) in [
        (&playlist, "hls", "manifest", "hls-aes128"),
        (&manifest, "dash", "manifest", "cenc"),
        (&mp4, "mp4", "container-metadata", "cenc"),
    ] {
        let original = std::fs::read(path).unwrap();
        let output = command()
            .args(["analyze", "--metadata-only", "--json"])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "metadata analysis must not need a tool or key"
        );
        let value = report(&output);
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["format"], format);
        assert_eq!(value["evidence"], evidence);
        assert_eq!(value["protection"], protection);
        assert_eq!(value["media_verified"], false);
        assert!(value.get("download").is_none());
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert!(output.stderr.is_empty(), "unexpected metadata diagnostics");
        no_secrets(&output);
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 3);
}

#[test]
fn metadata_and_light_planning_publish_structured_progress_separately() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source.m3u8");
    hls(&source);
    for (name, options) in [
        ("analyze", vec!["--metadata-only"]),
        ("plan", vec!["--to", "clear"]),
    ] {
        let output = command()
            .arg(name)
            .args(options)
            .arg(&source)
            .args(["--json", "--progress", "json"])
            .output()
            .unwrap();
        assert!(output.status.success());
        report(&output);
        let text = String::from_utf8_lossy(&output.stderr);
        let records: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("one JSON progress record"))
            .collect();
        assert!(!records.is_empty(), "progress records were absent");
        assert!(records.iter().all(|record| record["command"] == name));
        for activity in ["configuration", "inspection", "publication"] {
            assert!(records.iter().any(|record| record["activity"] == activity));
        }
        assert_eq!(records.last().unwrap()["event"], "finished");
        assert!(!text.contains(&source.to_string_lossy().into_owned()));
        no_secrets(&output);
    }
}

#[test]
fn metadata_and_light_plan_output_failures_keep_structured_stderr_and_originals() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source.m3u8");
    hls(&source);
    let original = std::fs::read(&source).unwrap();
    for (name, options) in [
        ("analyze", vec!["--metadata-only"]),
        ("plan", vec!["--to", "clear"]),
    ] {
        // Close the pipe before spawning, so publication deterministically fails.
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let output = command()
            .arg(name)
            .args(options)
            .arg(&source)
            .args(["--json", "--progress", "json"])
            .stdout(writer)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty(), "unexpected published bytes");
        let text = String::from_utf8_lossy(&output.stderr);
        let records: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("one JSON progress record"))
            .collect();
        assert!(records.iter().all(|record| record["command"] == name));
        let last = records.last().unwrap();
        assert_eq!(last["event"], "failed");
        assert_eq!(last["error_code"], "result-output-unavailable");
        assert_eq!(std::fs::read(&source).unwrap(), original);
        no_secrets(&output);
    }
}

#[test]
fn metadata_rejects_complete_analysis_options_including_an_explicit_default_tool() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source.m3u8");
    hls(&source);
    let path = root.path().join("metadata-key-sentinel");
    for args in [
        vec!["--download-dir", path.to_str().unwrap()],
        vec!["--keys-file", path.to_str().unwrap()],
        vec!["--packager-bin", "packager"],
        vec!["--max-workers", "1"],
        vec!["--decode"],
        vec!["--measure"],
    ] {
        let output = command()
            .args(["analyze", "--metadata-only", "--json"])
            .arg(&source)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!path.exists());
        no_secrets(&output);
    }
}

#[test]
fn local_metadata_refuses_fetch_settings_without_reading_them() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source.m3u8");
    hls(&source);
    let output = command()
        .args(["analyze", "--metadata-only", "--json"])
        .arg(&source)
        .arg("--fetch-policy")
        .arg(root.path().join("metadata-auth-sentinel"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        report(&output)["error"]["code"],
        "download-options-require-url"
    );
    no_secrets(&output);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
}

#[test]
fn metadata_errors_have_safe_json_without_echoing_input_names() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let output = command()
        .args(["analyze", "--metadata-only", "--json"])
        .arg(root.path().join("metadata-key-sentinel.mp4"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let value = report(&output);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["command"], "analyze");
    assert_eq!(value["status"], "failed");
    assert!(value["error"]["code"].is_string());
    no_secrets(&output);
}

#[test]
fn metadata_refuses_other_url_schemes_and_full_analysis_still_requires_retention() {
    let output = run(&[
        "analyze",
        "--metadata-only",
        "ftp://example.test/source?token=metadata-query-sentinel",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        report(&output)["error"]["code"],
        "download-url-scheme-not-supported"
    );
    no_secrets(&output);
    let output = run(&[
        "analyze",
        "https://example.test/source.mpd?token=metadata-query-sentinel",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        report(&output)["error"]["code"],
        "download-directory-required"
    );
    no_secrets(&output);
}

#[test]
fn removed_inspect_command_is_not_an_alias_or_a_help_entry() {
    let output = run(&["--help"]);
    assert!(output.status.success());
    assert!(
        !String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim_start().starts_with("inspect "))
    );
    // The removed command is intentionally invoked to prove that no alias remains.
    let output = run(&["inspect", "source.mp4"]);
    assert_eq!(output.status.code(), Some(2));
}

mod remote {
    use super::*;
    use std::{
        collections::BTreeMap,
        io::{Read as _, Write as _},
        net::{TcpListener, TcpStream},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };

    const AUTH: &str = "Bearer metadata-auth-sentinel";
    const QUERY: &str = "metadata-query-sentinel";

    struct Body {
        bytes: Vec<u8>,
        held: bool,
    }

    impl Body {
        fn bytes(bytes: Vec<u8>) -> Self {
            Self { bytes, held: false }
        }

        #[cfg(unix)]
        fn hold() -> Self {
            Self {
                bytes: Vec::new(),
                held: true,
            }
        }
    }

    #[derive(Clone)]
    struct Request {
        path: String,
        range: bool,
    }

    struct Server {
        origin: String,
        requests: Arc<Mutex<Vec<Request>>>,
        stop: Arc<AtomicBool>,
        worker: Option<JoinHandle<()>>,
    }

    impl Server {
        fn start(files: BTreeMap<String, Body>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let observed = requests.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = stop.clone();
            let worker = thread::spawn(move || {
                while !stopping.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((socket, _)) => {
                            let _ = serve(socket, &files, &observed, &stopping);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => {
                            panic!("metadata fixture listener failed: {:?}", error.kind())
                        }
                    }
                }
            });
            Self {
                origin,
                requests,
                stop,
                worker: Some(worker),
            }
        }

        fn url(&self, path: &str) -> String {
            format!("{}{path}?token={QUERY}", self.origin)
        }

        fn requests(&self) -> Vec<Request> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            self.worker.take().unwrap().join().unwrap();
        }
    }

    fn serve(
        mut socket: TcpStream,
        files: &BTreeMap<String, Body>,
        requests: &Mutex<Vec<Request>>,
        stop: &AtomicBool,
    ) -> std::io::Result<()> {
        socket.set_nonblocking(false)?;
        socket.set_read_timeout(Some(Duration::from_secs(1)))?;
        socket.set_write_timeout(Some(Duration::from_secs(1)))?;
        let mut bytes = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !bytes.ends_with(b"\r\n\r\n") {
            if bytes.len() == 16 * 1024 || Instant::now() >= deadline {
                return Err(std::io::Error::other("bounded metadata request"));
            }
            let mut byte = [0];
            if socket.read(&mut byte)? == 0 {
                return Ok(());
            }
            bytes.push(byte[0]);
        }
        let text = std::str::from_utf8(&bytes).map_err(std::io::Error::other)?;
        let target = text
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("GET "))
            .and_then(|line| line.strip_suffix(" HTTP/1.1"))
            .ok_or_else(|| std::io::Error::other("metadata request method"))?;
        let path = target.split('?').next().unwrap();
        let header = |name: &str| {
            text.lines().find_map(|line| {
                line.split_once(':')
                    .filter(|(field, _)| field.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value.trim())
            })
        };
        let range = header("range");
        requests.lock().unwrap().push(Request {
            path: path.into(),
            range: range.is_some(),
        });
        if header("authorization") != Some(AUTH) {
            return socket.write_all(
                b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
        let Some(body) = files.get(path) else {
            return socket.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        };
        if body.held {
            socket.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 65536\r\nConnection: close\r\n\r\n<MPD",
            )?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            return Ok(());
        }
        let body = &body.bytes;
        if let Some(range) = range {
            let (start, end) = range
                .strip_prefix("bytes=")
                .and_then(|value| value.split_once('-'))
                .and_then(|(start, end)| {
                    Some((
                        start.parse::<usize>().ok()?,
                        if end.is_empty() {
                            body.len().checked_sub(1)?
                        } else {
                            end.parse::<usize>().ok()?
                        },
                    ))
                })
                .ok_or_else(|| std::io::Error::other("metadata range"))?;
            if start >= body.len() || end < start {
                return socket.write_all(b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
            let end = end.min(body.len() - 1);
            write!(
                socket,
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len(),
                end - start + 1
            )?;
            socket.write_all(&body[start..=end])
        } else {
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            socket.write_all(body)
        }
    }

    fn policy(root: &Path, server: &Server) -> std::path::PathBuf {
        let path = root.join("policy.json");
        let value = serde_json::json!({
            "allow_http": true, "allow_private_network": true,
            "headers_by_origin": { server.origin.clone(): { "Authorization": AUTH } },
            "attempts": 1, "timeout": 1.0,
        });
        drmr_core::native::create_private_new(&path)
            .unwrap()
            .write_all(value.to_string().as_bytes())
            .unwrap();
        path
    }

    fn inspect(server: &Server, path: &str, policy: &Path, cwd: &Path) -> Output {
        command()
            .current_dir(cwd)
            .args([
                "analyze",
                "--metadata-only",
                "--json",
                &server.url(path),
                "--fetch-policy",
            ])
            .arg(policy)
            .output()
            .unwrap()
    }

    fn init() -> Vec<u8> {
        let source = include_bytes!("fixtures/bbb/cenc-iv8.mp4");
        let mut cursor = 0;
        let mut result = Vec::new();
        while cursor < source.len() {
            let mut size =
                u32::from_be_bytes(source[cursor..cursor + 4].try_into().unwrap()) as usize;
            let kind = &source[cursor + 4..cursor + 8];
            if size == 1 {
                size = u64::from_be_bytes(source[cursor + 8..cursor + 16].try_into().unwrap())
                    .try_into()
                    .unwrap();
            } else if size == 0 {
                size = source.len() - cursor;
            }
            assert!(size >= 8 && cursor + size <= source.len());
            if kind == b"ftyp" || kind == b"moov" {
                result.extend_from_slice(&source[cursor..cursor + size]);
            }
            cursor += size;
        }
        result
    }

    #[test]
    fn url_metadata_covers_three_formats_without_retention_media_keys_or_tools() {
        let hls = b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"key?token=metadata-key-sentinel\"\n#EXTINF:2,\nmedia.ts\n#EXT-X-ENDLIST\n".to_vec();
        let dash = b"<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" mediaPresentationDuration=\"PT2S\"><Period><AdaptationSet contentType=\"video\"><ContentProtection schemeIdUri=\"urn:mpeg:dash:mp4protection:2011\" value=\"cenc\"/><Representation id=\"video\" codecs=\"avc1.64001f\" bandwidth=\"500\"><SegmentTemplate initialization=\"init.mp4\" media=\"media-$Number$.m4s\" timescale=\"1\"><SegmentTimeline><S d=\"2\"/></SegmentTimeline></SegmentTemplate></Representation></AdaptationSet></Period></MPD>".to_vec();
        for (path, format, protection, body) in [
            ("/source.m3u8", "hls", "hls-aes128", hls),
            ("/source.mpd", "dash", "cenc", dash),
            (
                "/source.mp4",
                "mp4",
                "cenc",
                include_bytes!("fixtures/bbb/cenc-iv8.mp4").to_vec(),
            ),
        ] {
            let root = drmr_core::native::private_tempdir().unwrap();
            let server = Server::start(BTreeMap::from([
                (path.into(), Body::bytes(body)),
                ("/init.mp4".into(), Body::bytes(init())),
            ]));
            let policy = policy(root.path(), &server);
            let original_policy = std::fs::read(&policy).unwrap();
            let output = inspect(&server, path, &policy, root.path());
            assert!(
                output.status.success(),
                "URL metadata must not need a retained directory, key, or tool"
            );
            let value = report(&output);
            assert_eq!(value["format"], format);
            assert_eq!(value["protection"], protection);
            assert_eq!(value["media_verified"], false);
            assert!(value.get("download").is_none());
            assert!(output.stderr.is_empty(), "unexpected metadata diagnostics");
            no_secrets(&output);
            for bytes in [&output.stdout, &output.stderr] {
                assert!(!String::from_utf8_lossy(bytes).contains(&server.origin));
            }
            let requests = server.requests();
            assert!(!requests.is_empty());
            assert!(
                requests
                    .iter()
                    .all(|request| request.path == path || request.path == "/init.mp4")
            );
            if format == "mp4" {
                assert!(requests.iter().all(|request| request.range));
            }
            assert_eq!(std::fs::read(&policy).unwrap(), original_policy);
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        }
    }

    #[test]
    fn metadata_refuses_cross_origin_references_without_forwarding_authentication() {
        let root = drmr_core::native::private_tempdir().unwrap();
        let denied = Server::start(BTreeMap::new());
        let playlist = format!(
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000,CODECS=\"avc1.64001f\"\n{}/variant.m3u8\n",
            denied.origin
        );
        let server = Server::start(BTreeMap::from([(
            "/master.m3u8".into(),
            Body::bytes(playlist.into_bytes()),
        )]));
        let policy = policy(root.path(), &server);
        let output = inspect(&server, "/master.m3u8", &policy, root.path());
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            report(&output)["error"]["code"],
            "remote-metadata-unavailable"
        );
        assert!(denied.requests().is_empty());
        no_secrets(&output);
        for bytes in [&output.stdout, &output.stderr] {
            let shown = String::from_utf8_lossy(bytes);
            assert!(!shown.contains(&server.origin) && !shown.contains(&denied.origin));
        }
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_stops_metadata_fetch_and_leaves_no_retained_source() {
        struct OwnedChild(Option<std::process::Child>);
        impl Drop for OwnedChild {
            fn drop(&mut self) {
                if let Some(mut child) = self.0.take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let root = drmr_core::native::private_tempdir().unwrap();
        let server = Server::start(BTreeMap::from([("/source.mpd".into(), Body::hold())]));
        let policy = policy(root.path(), &server);
        let child = command()
            .current_dir(root.path())
            .args([
                "analyze",
                "--metadata-only",
                "--json",
                &server.url("/source.mpd"),
                "--fetch-policy",
            ])
            .arg(&policy)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut child = OwnedChild(Some(child));
        let deadline = Instant::now() + Duration::from_secs(3);
        while server.requests().is_empty() {
            assert!(
                child.0.as_mut().unwrap().try_wait().unwrap().is_none(),
                "metadata request did not start"
            );
            assert!(
                Instant::now() < deadline,
                "metadata request readiness deadline"
            );
            thread::sleep(Duration::from_millis(5));
        }
        rustix::process::kill_process(
            rustix::process::Pid::from_child(child.0.as_ref().unwrap()),
            rustix::process::Signal::TERM,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.0.as_mut().unwrap().try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "metadata cancellation deadline");
            thread::sleep(Duration::from_millis(10));
        }
        let output = child.0.take().unwrap().wait_with_output().unwrap();
        assert_eq!(output.status.code(), Some(130));
        assert_eq!(report(&output)["status"], "cancelled");
        assert_eq!(report(&output)["error"]["code"], "cancelled");
        no_secrets(&output);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
}
