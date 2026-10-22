//! URL handling and network-address policy.
use drmr_core::source::netaddr::is_global;
use drmr_core::source::pypath::as_uri;
use drmr_core::source::pyurl::{parse_ip, quote_from_bytes, unquote, urljoin, urlsplit};
use drmr_core::source::{Escaped, Failure, child_url, origin};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hls_source")
        .join(name)
}

fn load(name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(fixture(name)).unwrap()).unwrap()
}

fn attempt_string(result: Result<String, Failure>) -> Value {
    match result {
        Ok(text) => json!({"ok": text}),
        Err(Failure::Source(error)) => json!({"source_error": error.token()}),
        Err(Failure::Escaped(Escaped {
            class: "ValueError",
        })) => json!({"value_error": true}),
        Err(other) => panic!("{other:?}"),
    }
}

#[test]
fn urljoin_and_child_url_match_python() {
    let data = load("urls.json");
    for case in data["join"].as_array().unwrap() {
        let (base, reference) = (
            case["base"].as_str().unwrap(),
            case["ref"].as_str().unwrap(),
        );
        let joined = match urljoin(base, reference) {
            Ok(text) => json!({"ok": text}),
            Err(_) => json!({"value_error": true}),
        };
        assert_eq!(joined, case["urljoin"], "urljoin({base:?}, {reference:?})");
        assert_eq!(
            attempt_string(child_url(base, reference)),
            case["child_url"],
            "_child_url({base:?}, {reference:?})"
        );
    }
}

#[test]
fn urlsplit_origin_and_unquote_match_python() {
    let data = load("urls.json");
    for case in data["split"].as_array().unwrap() {
        let url = case["url"].as_str().unwrap();
        let split = match urlsplit(url, "") {
            Ok(parts) => json!({"ok": {
                "scheme": parts.scheme, "netloc": parts.netloc, "path": parts.path,
                "query": parts.query, "fragment": parts.fragment, "hostname": parts.hostname(),
                "username": parts.username(), "password": parts.password(),
                "port": match parts.port() { Ok(port) => json!(port), Err(_) => json!("ValueError") },
            }}),
            Err(_) => json!({"value_error": true}),
        };
        assert_eq!(split, case["split"], "urlsplit({url:?})");
        let origin = match origin(url) {
            Ok(text) => json!({"ok": text}),
            Err(error) => json!({"source_error": error.token()}),
        };
        assert_eq!(origin, case["origin"], "_origin({url:?})");
        let unquoted = match urlsplit(url, "") {
            Ok(parts) => json!({"ok": unquote(&parts.path)}),
            Err(_) => json!({"value_error": true}),
        };
        assert_eq!(unquoted, case["unquote_path"], "unquote({url:?})");
    }
}

#[test]
fn is_global_matches_python() {
    for case in load("is_global.json").as_array().unwrap() {
        let address = case["address"].as_str().unwrap();
        let ip = parse_ip(address).unwrap();
        assert_eq!(
            is_global(ip),
            case["global"].as_bool().unwrap(),
            "{address}"
        );
    }
}

#[test]
fn as_uri_and_quote_match_python() {
    for case in load("as_uri.json").as_array().unwrap() {
        let path = case["path"].as_str().unwrap();
        #[cfg(unix)]
        assert_eq!(
            as_uri(Path::new(path)).unwrap(),
            case["as_uri"].as_str().unwrap(),
            "{path}"
        );
        assert_eq!(
            quote_from_bytes(path.as_bytes()),
            case["quote"].as_str().unwrap(),
            "{path}"
        );
    }
}

#[cfg(windows)]
#[test]
fn native_file_uris_preserve_drive_names_and_encode_path_characters() {
    assert_eq!(
        as_uri(Path::new(r"C:\media\日本語 clip#1.ts")).unwrap(),
        "file:///C:/media/%E6%97%A5%E6%9C%AC%E8%AA%9E%20clip%231.ts"
    );
    assert_eq!(
        as_uri(Path::new("relative.ts")),
        Err(Escaped::new("ValueError"))
    );
}
