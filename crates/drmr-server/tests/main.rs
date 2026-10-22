mod support;

use drmr_server::config::configure;
use drmr_server::{JobCli, ServeArgs, config_from_env};
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};

fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
    let map: HashMap<String, OsString> = pairs
        .iter()
        .map(|(name, value)| ((*name).to_string(), OsString::from(value)))
        .collect();
    move |name: &str| map.get(name).cloned()
}

const ROOTS: [(&str, &str); 2] = [
    ("DRMR_INPUT_ROOT", "/srv/input"),
    ("DRMR_OUTPUT_ROOT", "/srv/output"),
];

fn with_roots(extra: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
    let mut pairs = ROOTS.to_vec();
    pairs.extend_from_slice(extra);
    pairs
}

#[test]
fn the_worker_bound_is_optional_and_read_from_the_environment() {
    assert_eq!(config_from_env(env(&ROOTS)).unwrap().max_workers, None);
    for (value, expected) in [("6", 6), ("64", 64), ("064", 64)] {
        let config = config_from_env(env(&with_roots(&[("DRMR_MAX_WORKERS", value)]))).unwrap();
        assert_eq!(config.max_workers, Some(expected), "{value}");
    }
}

#[test]
fn an_invalid_worker_bound_stops_the_tool_at_startup() {
    for value in ["0", "-1", "65", "many", "2.5", " 3", "\u{b2}", "000"] {
        let error = config_from_env(env(&with_roots(&[("DRMR_MAX_WORKERS", value)]))).unwrap_err();
        assert_eq!(error.exit_code, 2, "{value:?}");
        assert!(error.message.contains("DRMR_MAX_WORKERS"), "{value:?}");
    }
}

#[test]
fn missing_roots_stop_the_tool_at_startup() {
    for pairs in [
        vec![],
        vec![ROOTS[0]],
        vec![ROOTS[1]],
        vec![ROOTS[0], ("DRMR_OUTPUT_ROOT", "")],
    ] {
        let error = config_from_env(env(&pairs)).unwrap_err();
        assert_eq!(error.exit_code, 2);
        assert_eq!(
            error.message,
            "error: DRMR_INPUT_ROOT and DRMR_OUTPUT_ROOT must both be set"
        );
    }
}

#[test]
fn the_defaults_and_the_remaining_environment_variables() {
    let config = config_from_env(env(&ROOTS)).unwrap();
    assert_eq!(
        (
            config.bind_host.as_str(),
            config.bind_port,
            config.common_config_root
        ),
        ("127.0.0.1", 8080, None)
    );
    let config = config_from_env(env(&with_roots(&[
        ("DRMR_BIND_HOST", "0.0.0.0"),
        ("DRMR_BIND_PORT", " 9_090 "),
        ("DRMR_COMMON_CONFIG_ROOT", "/srv/private"),
    ])))
    .unwrap();
    assert_eq!(
        (config.bind_host.as_str(), config.bind_port),
        ("0.0.0.0", 9090)
    );
    assert_eq!(
        config.common_config_root.unwrap().to_str(),
        Some("/srv/private")
    );
    assert!(
        config_from_env(env(&with_roots(&[("DRMR_COMMON_CONFIG_ROOT", "")])))
            .unwrap()
            .common_config_root
            .is_none()
    );
    let bad = config_from_env(env(&with_roots(&[("DRMR_BIND_PORT", "http")]))).unwrap_err();
    assert_eq!(
        (bad.exit_code, bad.message.as_str()),
        (
            1,
            "ValueError: invalid literal for int() with base 10: 'http'"
        )
    );
    let high = config_from_env(env(&with_roots(&[("DRMR_BIND_PORT", "65536")]))).unwrap_err();
    assert_eq!(high.exit_code, 1);
}

#[test]
fn the_jobs_run_the_hosting_executables_cli_unless_another_is_named() {
    let default = JobCli {
        argv: vec![OsString::from("/opt/drmr/bin/drmr")],
    };
    let config = configure(env(&ROOTS), &ServeArgs::default(), default.clone()).unwrap();
    assert_eq!(config.cli_argv, default.argv);
    assert_eq!(config.static_dir, None);
    let named = ServeArgs {
        static_dir: Some("/srv/static".into()),
        cli: Some("/opt/other/drmr".into()),
    };
    let config = configure(env(&ROOTS), &named, default.clone()).unwrap();
    assert_eq!(config.cli_argv, vec![OsString::from("/opt/other/drmr")]);
    assert_eq!(config.static_dir, named.static_dir);
    assert_eq!(
        configure(env(&[]), &named, default).unwrap_err().exit_code,
        2
    );
}
#[test]
fn drmr_serve_runs_itself_and_drmr_server_its_sibling() {
    assert_eq!(
        JobCli::this_executable().argv,
        vec![std::env::current_exe().unwrap().into_os_string()]
    );
    assert_eq!(
        JobCli::sibling().argv,
        drmr_server::config::default_cli_argv()
    );
}

#[test]
fn the_binary_refuses_to_start_without_its_roots() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_drmr-server"))
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

#[test]
fn the_binary_serves_the_embedded_page_and_the_api_on_the_configured_port() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_drmr-server"))
        .env("DRMR_INPUT_ROOT", &input)
        .env("DRMR_OUTPUT_ROOT", dir.path().join("output"))
        .env("DRMR_BIND_PORT", "0")
        .env_remove("DRMR_BIND_HOST")
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut banner = String::new();
    stderr.read_line(&mut banner).unwrap();
    let address = banner
        .trim()
        .strip_prefix("* Running on http://")
        .map(str::to_string);
    let fetch = |path: &str| -> String {
        let mut stream = std::net::TcpStream::connect(address.as_deref().unwrap()).unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        answer
    };
    let capabilities_answer = address.as_ref().map(|_| fetch("/api/common/capabilities"));
    let page = address.as_ref().map(|_| fetch("/"));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        address
            .as_deref()
            .is_some_and(|a| a.starts_with("127.0.0.1:")),
        "loopback by default: {banner:?}"
    );
    let capabilities_answer = capabilities_answer.unwrap();
    assert!(
        capabilities_answer.starts_with("HTTP/1.1 200"),
        "{capabilities_answer}"
    );
    let payload: serde_json::Value =
        serde_json::from_str(capabilities_answer.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(payload["schema_version"], 1);
    assert_eq!(payload["execution"]["availability"], "idle");
    let page = page.unwrap();
    let index =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/static/index.html")).unwrap();
    assert!(
        page.starts_with("HTTP/1.1 200") && page.ends_with(&index),
        "the embedded page is served"
    );
}

const PIPE_CLOSE_PROBES: usize = 32;

fn wait_for_broken_pipe(
    writer: &mut std::io::PipeWriter,
    deadline: std::time::Instant,
) -> std::io::Result<()> {
    for attempt in 0..PIPE_CLOSE_PROBES {
        match writer.write(&[0]) {
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
            Err(error) => return Err(error),
            Ok(1) => {}
            Ok(_) => return Err(std::io::ErrorKind::WriteZero.into()),
        }
        if attempt + 1 == PIPE_CLOSE_PROBES || std::time::Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "stderr pipe still has a reader",
            ));
        }
        std::thread::sleep(
            std::time::Duration::from_millis(10)
                .min(deadline.saturating_duration_since(std::time::Instant::now())),
        );
    }
    unreachable!("a successful final probe exhausts the bounded attempts")
}

struct OwnedServer(std::process::Child);

impl Drop for OwnedServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_pipe_barrier_waits_for_the_actual_reader_to_close() {
    let (mut reader, mut writer) = std::io::pipe().unwrap();
    let reader = std::thread::spawn(move || {
        let mut byte = [255];
        let result = reader.read_exact(&mut byte);
        drop(reader);
        result.map(|()| byte)
    });
    let result = wait_for_broken_pipe(
        &mut writer,
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    );
    drop(writer);
    let observed = reader.join().unwrap();
    result.unwrap();
    assert_eq!(observed.unwrap(), [0]);
}

#[test]
fn an_open_reader_cannot_pass_an_exhausted_pipe_deadline() {
    let (mut reader, mut writer) = std::io::pipe().unwrap();
    let error = wait_for_broken_pipe(&mut writer, std::time::Instant::now()).unwrap_err();
    drop(writer);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(
        bytes,
        [0],
        "the actual write succeeded but did not prove closure"
    );
}

#[test]
fn an_open_reader_exhausts_the_pipe_probe_bound() {
    let (mut reader, mut writer) = std::io::pipe().unwrap();
    let error = wait_for_broken_pipe(
        &mut writer,
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    )
    .unwrap_err();
    drop(writer);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(!bytes.is_empty() && bytes.len() <= PIPE_CLOSE_PROBES);
}

#[test]
fn a_closed_stderr_returns_an_internal_error_page() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input");
    std::fs::create_dir(&input).unwrap();
    let (reader, writer) = std::io::pipe().unwrap();
    let mut probe = writer.try_clone().unwrap();
    let mut child = OwnedServer(
        std::process::Command::new(env!("CARGO_BIN_EXE_drmr-server"))
            .env("DRMR_INPUT_ROOT", &input)
            .env("DRMR_OUTPUT_ROOT", dir.path().join("output"))
            .env("DRMR_BIND_PORT", "0")
            .env_remove("DRMR_BIND_HOST")
            .stderr(writer)
            .spawn()
            .unwrap(),
    );
    let mut stderr = BufReader::new(reader);
    let mut banner = String::new();
    stderr.read_line(&mut banner).unwrap();
    let address = banner
        .trim()
        .strip_prefix("* Running on http://")
        .unwrap_or_default()
        .to_string();
    let fetch = |path: &str| -> String {
        let mut stream = std::net::TcpStream::connect(&address).unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        answer
    };
    let open = fetch("/api/common/capabilities");
    drop(stderr);
    // A concurrent spawn can temporarily retain another reader. Establish the
    // kernel's actual BrokenPipe state before asserting any HTTP error response.
    // At most 32 single bytes are written, well below this otherwise idle pipe's capacity.
    wait_for_broken_pipe(
        &mut probe,
        std::time::Instant::now() + std::time::Duration::from_secs(1),
    )
    .unwrap();
    let answers: Vec<String> = [
        "/api/common/capabilities",
        "/api/common/sessions",
        "/api/common/capabilities",
    ]
    .iter()
    .map(|path| fetch(path))
    .collect();
    let still_running = child.0.try_wait().unwrap().is_none();
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(open.starts_with("HTTP/1.1 200"), "{open}");
    for answer in &answers {
        assert!(answer.starts_with("HTTP/1.1 500"), "{answer}");
        assert!(
            answer.contains("<title>500 Internal Server Error</title>"),
            "{answer}"
        );
        let head = answer
            .split("\r\n\r\n")
            .next()
            .unwrap()
            .to_ascii_lowercase();
        assert!(
            head.contains("content-type: text/html; charset=utf-8"),
            "{answer}"
        );
        assert!(
            !head.contains("cache-control"),
            "no after_request header: {answer}"
        );
    }
    assert!(still_running, "the server keeps serving");
}
