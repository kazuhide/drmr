//! Batch configuration adapters and `CPython`-compatible string and URL semantics.
#![cfg(unix)] // the modules under test exist on unix only (open decision #23)

use drmr_core::batch::{AttemptError, HlsSource};
use drmr_core::batch_support::{
    DefaultHlsSource, as_uri, is_relative_to, online_cpus, py_bytes_splitlines, py_float,
    py_fromhex, py_int, py_path, py_splitlines, py_strip, py_truthy, quote_from_bytes, resolve,
    unquote, unquote_plus, urljoin, urlsplit,
};
use drmr_core::config::{KeyMaterial, load_key_material};
use drmr_core::hls_cli::private_json;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn oracle() -> Value {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/batch/python_semantics.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn rows<'a>(oracle: &'a Value, name: &str) -> &'a Vec<Value> {
    oracle[name].as_array().unwrap()
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap()
}

#[test]
fn the_oracle_is_cpython_3_11() {
    assert!(text(&oracle()["python"]).starts_with("3.11."));
}

#[test]
fn urljoin_matches_python_3_11() {
    let oracle = oracle();
    for row in rows(&oracle, "urljoin") {
        let (base, reference, expected) = (text(&row[0]), text(&row[1]), text(&row[2]));
        assert_eq!(
            urljoin(base, reference).unwrap(),
            expected,
            "urljoin({base:?}, {reference:?})"
        );
    }
}

#[test]
fn urlsplit_matches_python_3_11() {
    let oracle = oracle();
    for row in rows(&oracle, "urlsplit") {
        let url = text(&row[0]);
        let split = urlsplit(url, "").unwrap();
        let actual = json!([
            url,
            split.scheme,
            split.netloc,
            split.path,
            split.query,
            split.fragment,
            split.username(),
            split.password(),
            split.hostname()
        ]);
        assert_eq!(&actual, row, "urlsplit({url:?})");
    }
    for row in rows(&oracle, "urlsplit_errors") {
        let url = text(&row[0]);
        let outcome = if urlsplit(url, "").is_ok() {
            "ok"
        } else {
            "ValueError"
        };
        assert_eq!(outcome, text(&row[1]), "urlsplit({url:?})");
    }
}

#[test]
fn unquote_and_quote_match_python() {
    let oracle = oracle();
    for row in rows(&oracle, "unquote") {
        assert_eq!(
            unquote(text(&row[0])),
            text(&row[1]),
            "unquote({:?})",
            row[0]
        );
    }
    for row in rows(&oracle, "unquote_plus") {
        assert_eq!(unquote_plus(text(&row[0])), text(&row[1]));
    }
    for row in rows(&oracle, "quote_from_bytes") {
        let bytes: Vec<u8> = row[0]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| u8::try_from(b.as_u64().unwrap()).unwrap())
            .collect();
        assert_eq!(quote_from_bytes(&bytes, b"/"), text(&row[1]));
    }
    for row in rows(&oracle, "as_uri") {
        assert_eq!(as_uri(Path::new(text(&row[0]))).unwrap(), text(&row[1]));
    }
}

#[test]
fn splitlines_and_strip_match_python() {
    let oracle = oracle();
    for row in rows(&oracle, "splitlines") {
        let expected: Vec<&str> = row[1].as_array().unwrap().iter().map(text).collect();
        assert_eq!(
            py_splitlines(text(&row[0])),
            expected,
            "{:?}.splitlines()",
            row[0]
        );
    }
    for row in rows(&oracle, "bytes_splitlines") {
        let bytes: Vec<u8> = serde_json::from_value(row[0].clone()).unwrap();
        let expected: Vec<Vec<u8>> = serde_json::from_value(row[1].clone()).unwrap();
        let actual: Vec<Vec<u8>> = py_bytes_splitlines(&bytes)
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();
        assert_eq!(actual, expected);
    }
    for row in rows(&oracle, "strip") {
        assert_eq!(
            py_strip(text(&row[0])),
            text(&row[1]),
            "{:?}.strip()",
            row[0]
        );
    }
}

#[test]
fn fromhex_int_float_and_pathlib_match_python() {
    let oracle = oracle();
    for row in rows(&oracle, "fromhex") {
        let expected: Option<Vec<u8>> = serde_json::from_value(row[1].clone()).unwrap();
        assert_eq!(
            py_fromhex(text(&row[0])),
            expected,
            "bytes.fromhex({:?})",
            row[0]
        );
    }
    for row in rows(&oracle, "int") {
        let actual = py_int(text(&row[0])).ok();
        match row[1].as_i64() {
            Some(expected) => assert_eq!(actual, Some(expected), "int({:?})", row[0]),
            // Out of i64 range: accepted and saturated (every caller bounds it far lower).
            None if !row[1].is_null() => assert_eq!(actual, Some(i64::MAX)),
            None => assert_eq!(actual, None, "int({:?})", row[0]),
        }
    }
    for row in rows(&oracle, "float") {
        let actual = py_float(text(&row[0])).ok().map(float_repr);
        let expected = row[1].as_str().map(str::to_string);
        assert_eq!(actual, expected, "float({:?})", row[0]);
    }
    for row in rows(&oracle, "pathlib") {
        assert_eq!(
            py_path(OsStr::new(text(&row[0]))),
            Path::new(text(&row[1])),
            "Path({:?})",
            row[0]
        );
    }
}

/// `repr(float)`, including the non-finite values JSON never carries.
fn float_repr(value: f64) -> String {
    if value.is_nan() {
        "nan".to_string()
    } else if value.is_infinite() {
        if value > 0.0 { "inf" } else { "-inf" }.to_string()
    } else {
        drmr_core::canonical::python_float_repr(value)
    }
}

#[test]
fn truthiness_is_python_s() {
    for falsy in [
        json!(null),
        json!(false),
        json!(0),
        json!(0.0),
        json!(""),
        json!([]),
        json!({}),
    ] {
        assert!(!py_truthy(&falsy), "{falsy}");
    }
    for truthy in [
        json!(true),
        json!(1),
        json!(-0.5),
        json!("x"),
        json!([0]),
        json!({"a": 0}),
    ] {
        assert!(py_truthy(&truthy), "{truthy}");
    }
}

#[test]
fn resolve_follows_links_of_existing_components_and_keeps_the_rest() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    std::fs::create_dir_all(root.join("real/inner")).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("alias")).unwrap();
    assert_eq!(
        resolve(&root.join("alias/inner")).unwrap(),
        root.join("real/inner")
    );
    assert_eq!(
        resolve(&root.join("alias/missing/x")).unwrap(),
        root.join("real/missing/x")
    );
    assert_eq!(
        resolve(&root.join("alias/../real")).unwrap(),
        root.join("real")
    );
    assert_eq!(
        resolve(&root.join("missing/../real")).unwrap(),
        root.join("real")
    );
    std::os::unix::fs::symlink("loop", root.join("loop")).unwrap();
    // A loop resolves to the link itself plus the rest (realpath, non-strict).
    assert_eq!(resolve(&root.join("loop/x")).unwrap(), root.join("loop/x"));
    assert!(is_relative_to(&root.join("real/inner"), &root.join("real")));
    assert!(!is_relative_to(&root.join("realm"), &root.join("real")));
    assert!(resolve(Path::new("relative")).unwrap().is_absolute());
}

#[test]
fn key_material_validates_16_byte_hex_and_never_debug_prints_values() {
    let kid = "000102030405060708090a0b0c0d0e0f";
    let key = "10101010101010101010101010101010";
    let iv = "20202020202020202020202020202020";
    let material = KeyMaterial::new(kid, key, iv).unwrap();
    let shown = format!("{material:?}");
    assert!(
        !shown.contains(kid) && !shown.contains(key) && !shown.contains(iv),
        "{shown}"
    );
    assert!(KeyMaterial::new(kid, "1010", iv).is_err());
    assert!(KeyMaterial::new(kid, key, "zz202020202020202020202020202020").is_err());
    // bytes.fromhex accepts white space between pairs.
    assert!(KeyMaterial::new("00 01 02 03 04 05 06 07 08 09 0a 0b 0c 0d 0e 0f", key, iv).is_ok());
    let mut env = HashMap::new();
    env.insert("DRM_KID".to_string(), kid.to_string());
    env.insert("DRM_KEY".to_string(), key.to_string());
    env.insert("DRM_IV".to_string(), iv.to_string());
    assert_eq!(load_key_material(Some(&env)).unwrap(), material);
    env.insert("DRM_IV".to_string(), String::new());
    let error = load_key_material(Some(&env)).unwrap_err();
    assert!(!error.to_string().contains(key));
    // A value that is not text never reaches `load_key_material` from the batch layer: the key
    // file's rows refuse it (`key_files_are_refused_under_the_output_root_or_malformed`).
    env.insert("DRM_IV".to_string(), "5".to_string());
    assert!(load_key_material(Some(&env)).is_err());
}

#[test]
fn private_json_requires_an_owner_only_json_object() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("private.json");
    std::fs::write(
        &path,
        br#"{"url": "https://private.invalid/?token=NEVER_ECHO_THIS"}"#,
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        private_json(&path).unwrap()["url"],
        json!("https://private.invalid/?token=NEVER_ECHO_THIS")
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(
        private_json(&path).unwrap_err().token(),
        "private-config-invalid"
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&path, b"[1, 2]").unwrap();
    assert_eq!(
        private_json(&path).unwrap_err().token(),
        "private-config-invalid"
    );
}

#[test]
fn online_cpu_lists_count_like_sysconf() {
    assert_eq!(online_cpus("0-3\n"), Some(4));
    assert_eq!(online_cpus("0-3,5,7-8"), Some(7));
    assert_eq!(online_cpus("0"), Some(1));
    assert_eq!(online_cpus(""), None);
    assert!(drmr_core::batch_support::cpu_count() >= 1);
}

#[test]
fn default_source_lines_child_urls_and_policies_are_the_source_port() {
    let source = DefaultHlsSource;
    assert_eq!(
        source
            .lines(b"\xef\xbb\xbf#EXTM3U\r\n\r\n#EXTINF:6,\n s.ts \n")
            .unwrap(),
        ["#EXTM3U", "#EXTINF:6,", "s.ts"]
    );
    let refused = |token: &str| AttemptError::Value(token.to_string());
    assert_eq!(
        source.lines(b"#EXTM3U\n#EXT-X-UNKNOWN:1\n").unwrap_err(),
        refused("unsupported-playlist-tag")
    );
    assert_eq!(
        source.lines(b"\xff").unwrap_err(),
        refused("invalid-playlist")
    );
    assert_eq!(
        source.lines(b"s.ts\n").unwrap_err(),
        refused("invalid-playlist")
    );
    let base = "https://cdn.test/a/master.m3u8";
    assert_eq!(
        source.child_url(base, "v/media.m3u8").unwrap(),
        "https://cdn.test/a/v/media.m3u8"
    );
    assert_eq!(
        source.child_url(base, "file:///etc/hosts").unwrap_err(),
        refused("source-not-allowed")
    );
    assert_eq!(
        source.child_url(base, "a\u{1}b").unwrap_err(),
        refused("source-not-allowed")
    );
    // `hls_cli.fetch_policy(policy)`: `FetchPolicy(**dict(policy))` with Python's `dict`
    // forms (`""`, `[key, value]` pairs, a two-key object as a pair of its keys), every refusal
    // `invalid-fetch-policy` (the Rust tool's `max_total_resources` is an unknown keyword to
    // Python, whatever its value).
    let temp = tempfile::tempdir().unwrap();
    for policy in [
        json!({}),
        json!(""),
        json!([["connections", 2]]),
        json!([{"allow_http": null, "y": null}]),
    ] {
        assert!(
            source.open_fetcher(&policy, temp.path()).is_ok(),
            "{policy}"
        );
    }
    for policy in [
        json!(null),
        json!("ab"),
        json!([["connections"]]),
        json!({"bogus": 1}),
        json!({"max_total_resources": 9}),
        json!({"max_total_resources": null}),
        json!([["max_total_resources", 9]]),
        json!(["ab"]),
        json!({"attempts": 6}),
        json!({"connections": 65}),
        json!({"timeout": true}),
    ] {
        assert_eq!(
            source.open_fetcher(&policy, temp.path()).err(),
            Some(refused("invalid-fetch-policy")),
            "{policy}"
        );
    }
}

#[test]
fn default_source_reads_admitted_local_playlists_and_verified_snapshots() {
    let temp = tempfile::tempdir().unwrap();
    let master = resolve(temp.path()).unwrap().join("master.m3u8");
    std::fs::write(
        &master,
        b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nmedia.m3u8\n",
    )
    .unwrap();
    let location = as_uri(&master).unwrap();
    let root = resolve(temp.path()).unwrap();
    let policy = json!({"allowed_local_roots": [root.to_str().unwrap()]});
    let mut fetcher = DefaultHlsSource
        .open_fetcher(&policy, temp.path())
        .ok()
        .unwrap();
    let (body, effective) = fetcher.read_playlist(&location).unwrap();
    assert_eq!(body, std::fs::read(&master).unwrap());
    assert_eq!(effective, location);
    fetcher.close_all();
    let mut outside = DefaultHlsSource
        .open_fetcher(&json!({}), temp.path())
        .ok()
        .unwrap();
    assert_eq!(
        outside.read_playlist(&location).unwrap_err(),
        AttemptError::Value("source-not-allowed".to_string())
    );
    // `read_analysis`: a record that is not an object escapes as an `AttributeError`; one
    // without the snapshot fields, or no record at all, is a `SourceError`.
    let analysis = temp.path().join("analysis.json");
    std::fs::write(&analysis, b"[]").unwrap();
    assert_eq!(
        DefaultHlsSource.read_analysis(&analysis).unwrap_err(),
        AttemptError::Other
    );
    std::fs::write(&analysis, b"{}").unwrap();
    assert_eq!(
        DefaultHlsSource.read_analysis(&analysis).unwrap_err(),
        AttemptError::Value("snapshot-invalid".to_string())
    );
    std::fs::remove_file(&analysis).unwrap();
    assert_eq!(
        DefaultHlsSource.read_analysis(&analysis).unwrap_err(),
        AttemptError::Value("snapshot-invalid".to_string())
    );
}
