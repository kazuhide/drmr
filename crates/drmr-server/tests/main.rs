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
    ("DRMR_CATALOG_ROOT", "/srv/catalog"),
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
            "error: DRMR_CATALOG_ROOT and DRMR_OUTPUT_ROOT must both be set"
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
            config.hls_profiles.clone()
        ),
        ("127.0.0.1", 8080, None)
    );
    let config = config_from_env(env(&with_roots(&[
        ("DRMR_BIND_HOST", "0.0.0.0"),
        ("DRMR_BIND_PORT", " 9_090 "),
        ("DRMR_HLS_PROFILES", "/srv/profiles.json"),
    ])))
    .unwrap();
    assert_eq!(
        (config.bind_host.as_str(), config.bind_port),
        ("0.0.0.0", 9090),
        "int() accepts spaces and underscores"
    );
    assert_eq!(
        config.hls_profiles.unwrap().to_str(),
        Some("/srv/profiles.json")
    );
    let empty = config_from_env(env(&with_roots(&[("DRMR_HLS_PROFILES", "")]))).unwrap();
    assert!(
        empty.hls_profiles.is_none(),
        "an empty value means no profiles"
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
        has_batch: true,
    };
    let config = configure(env(&ROOTS), &ServeArgs::default(), default.clone()).unwrap();
    assert_eq!(config.cli_argv, default.argv);
    assert!(config.batch_available, "the default CLI carries batch");
    assert_eq!(config.static_dir, None);

    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("other-cli");
    std::fs::write(&other, b"").unwrap();
    let named = ServeArgs {
        static_dir: Some(dir.path().join("static")),
        cli: Some(other.clone()),
    };
    let config = configure(env(&ROOTS), &named, default.clone()).unwrap();
    assert_eq!(config.cli_argv, vec![other.clone().into_os_string()]);
    assert!(
        !config.batch_available,
        "another program is not vouched for"
    );
    assert_eq!(config.static_dir, named.static_dir);

    // Naming the default CLI's own file, here through a symlink, keeps what it carries.
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&other, &link).unwrap();
    let own = JobCli {
        argv: vec![other.into_os_string()],
        has_batch: true,
    };
    let through_link = ServeArgs {
        static_dir: None,
        cli: Some(link.clone()),
    };
    let config = configure(env(&ROOTS), &through_link, own).unwrap();
    assert_eq!(config.cli_argv, vec![link.into_os_string()]);
    assert!(config.batch_available);

    let refused = configure(env(&[]), &named, default).unwrap_err();
    assert_eq!(refused.exit_code, 2);
}

#[test]
fn drmr_serve_runs_itself_and_drmr_server_its_sibling() {
    let itself = JobCli::this_executable(true);
    assert_eq!(
        itself.argv,
        vec![std::env::current_exe().unwrap().into_os_string()]
    );
    assert!(itself.has_batch);
    assert!(!JobCli::this_executable(false).has_batch);
    let sibling = JobCli::sibling();
    assert_eq!(sibling.argv, drmr_server::config::default_cli_argv());
    assert!(!sibling.has_batch, "a separate program is not vouched for");
}

#[test]
fn the_binary_refuses_to_start_without_its_roots() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_drmr-server"))
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

#[test]
fn the_binary_serves_the_embedded_page_and_the_api_on_the_configured_port() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    std::fs::create_dir(&catalog).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_drmr-server"))
        .env("DRMR_CATALOG_ROOT", &catalog)
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
    let catalog_answer = address.as_ref().map(|_| fetch("/api/catalog"));
    let page = address.as_ref().map(|_| fetch("/"));
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        address
            .as_deref()
            .is_some_and(|a| a.starts_with("127.0.0.1:")),
        "loopback by default: {banner:?}"
    );
    let catalog_answer = catalog_answer.unwrap();
    assert!(
        catalog_answer.starts_with("HTTP/1.1 200"),
        "{catalog_answer}"
    );
    assert!(catalog_answer.ends_with("\r\n\r\n[]\n"), "{catalog_answer}");
    let page = page.unwrap();
    let index =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/static/index.html")).unwrap();
    assert!(
        page.starts_with("HTTP/1.1 200") && page.ends_with(&index),
        "the embedded page is served"
    );
}

#[test]
fn a_closed_stderr_returns_an_internal_error_page() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("catalog");
    std::fs::create_dir(&catalog).unwrap();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_drmr-server"))
        .env("DRMR_CATALOG_ROOT", &catalog)
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
    let open = fetch("/api/catalog");
    drop(stderr); // the reader goes away: the server's stderr is now a closed pipe
    let answers: Vec<String> = ["/api/catalog", "/api/hls/jobs", "/api/hls/jobs"]
        .iter()
        .map(|path| fetch(path))
        .collect();
    let still_running = child.try_wait().unwrap().is_none();
    child.kill().unwrap();
    child.wait().unwrap();
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
