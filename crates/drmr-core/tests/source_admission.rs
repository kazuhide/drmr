//! Fetch-policy limits and local path resolution.
#[cfg(unix)]
use drmr_core::source::Escaped;
use drmr_core::source::FetchPolicy;
use serde_json::json;

#[test]
fn fetch_integer_limits_cannot_disable_bounds() {
    // Python passes nan, inf, 1.5 and True; a JSON policy cannot hold nan or inf (the reader
    // refuses the literal), and the typed struct cannot hold any of them.
    for field in [
        "max_total_bytes",
        "max_resource_bytes",
        "max_playlist_bytes",
        "max_playlists",
        "max_segments_per_track",
        "attempts",
        "min_free_disk_bytes",
    ] {
        for value in [
            json!(1.5),
            json!(true),
            json!(f64::MAX),
            json!(-1),
            json!("1"),
        ] {
            let error = FetchPolicy::from_json(&json!({ field: value })).unwrap_err();
            assert_eq!(error.token(), "invalid-fetch-policy", "{field} = {value}");
        }
    }
    for field in [
        "max_total_bytes",
        "max_resource_bytes",
        "max_playlist_bytes",
        "max_playlists",
        "max_segments_per_track",
        "attempts",
    ] {
        let error = FetchPolicy::from_json(&json!({ field: 0 })).unwrap_err();
        assert_eq!(error.token(), "invalid-fetch-policy", "{field} = 0");
    }
    assert!(FetchPolicy::from_json(&json!({"min_free_disk_bytes": 0})).is_ok());
    assert_eq!(
        FetchPolicy::from_json(&json!({"attempts": 6}))
            .unwrap_err()
            .token(),
        "invalid-fetch-policy"
    );
    assert_eq!(
        FetchPolicy::from_json(&json!({"unknown": 1}))
            .unwrap_err()
            .token(),
        "invalid-fetch-policy"
    );
}

#[test]
fn fetch_timeout_requires_finite_positive_number() {
    for timeout in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
        let policy = FetchPolicy {
            timeout,
            ..FetchPolicy::default()
        };
        assert_eq!(
            policy.validate().unwrap_err().token(),
            "invalid-fetch-policy",
            "{timeout}"
        );
    }
    for value in [json!(true), json!(0), json!(-1), json!("30")] {
        let error = FetchPolicy::from_json(&json!({ "timeout": value })).unwrap_err();
        assert_eq!(error.token(), "invalid-fetch-policy", "{value}");
    }
    assert_eq!(
        FetchPolicy::from_json(&json!({"timeout": 0.5}))
            .unwrap()
            .timeout
            .to_bits(),
        0.5_f64.to_bits()
    );
    assert_eq!(
        FetchPolicy::from_json(&json!({"timeout": 2}))
            .unwrap()
            .timeout
            .to_bits(),
        2.0_f64.to_bits()
    );
}

#[test]
fn connection_bounds_are_positive_bounded_integers() {
    for value in [0, 65] {
        let policy = FetchPolicy {
            connections: value,
            ..FetchPolicy::default()
        };
        assert_eq!(
            policy.validate().unwrap_err().token(),
            "invalid-fetch-policy",
            "connections {value}"
        );
    }
    for value in [json!(0), json!(65), json!(1.5), json!(true)] {
        let error = FetchPolicy::from_json(&json!({ "connections": value })).unwrap_err();
        assert_eq!(
            error.token(),
            "invalid-fetch-policy",
            "connections = {value}"
        );
    }
    let bounds = FetchPolicy::from_json(&json!({"connections": 64})).unwrap();
    assert_eq!(bounds.connections, 64);
}

#[test]
fn fetch_connections_default_to_sixteen() {
    assert_eq!(FetchPolicy::default().connections, 16);
}

#[test]
fn fetch_policy_rejects_the_removed_probe_worker_setting() {
    for value in [json!(null), json!(1), json!(64)] {
        assert_eq!(
            FetchPolicy::from_json(&json!({ "probe_workers": value }))
                .unwrap_err()
                .token(),
            "invalid-fetch-policy"
        );
    }
}

#[test]
#[cfg(unix)]
fn resolve_follows_3_11s_path_resolve() {
    use drmr_core::source::pypath::resolve;
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("dir/sub")).unwrap();
    std::fs::write(root.join("dir/file"), b"x").unwrap();
    symlink("dir", root.join("link")).unwrap();
    symlink("loop", root.join("loop")).unwrap();
    symlink("missing", root.join("dangling")).unwrap();
    symlink("l2", root.join("l1")).unwrap();
    symlink("l1", root.join("l2")).unwrap();
    let ok = |relative: &str| resolve(&root.join(relative)).unwrap();
    assert_eq!(ok("dir//sub/./"), root.join("dir/sub"));
    assert_eq!(ok("missing/x/../y"), root.join("missing/y"));
    assert_eq!(ok("link/sub/.."), root.join("dir"));
    assert_eq!(ok("dangling/x"), root.join("missing/x"));
    assert_eq!(ok("dir/file/x"), root.join("dir/file/x"));
    assert_eq!(ok("dir/file/.."), root.join("dir"));
    // the `stat` swallows every error but `ELOOP`; `loop/..` leaves the loop behind
    assert_eq!(ok("loop/.."), root);
    for looping in ["loop", "loop/x", "l1", "l1/x"] {
        assert_eq!(
            resolve(&root.join(looping)),
            Err(Escaped::new("RuntimeError")),
            "{looping}"
        );
    }
}
