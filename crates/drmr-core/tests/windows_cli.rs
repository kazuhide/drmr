//! Windows configuration and local-source command contracts.
#![cfg(windows)]

use drmr_core::{hls_cli, native};
use std::io::Write;

#[test]
fn private_json_accepts_a_user_only_file_and_refuses_inherited_access() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("private 日本語");
    native::create_private_dir_all(&directory).unwrap();
    let path = directory.join("config.json");
    native::create_private_new(&path)
        .unwrap()
        .write_all(br#"{"policy": {}}"#)
        .unwrap();
    assert!(hls_cli::private_json(&path).unwrap()["policy"].is_object());
    let inherited = temp.path().join("inherited.json");
    std::fs::write(&inherited, br#"{"policy": {}}"#).unwrap();
    assert_eq!(
        hls_cli::private_json(&inherited).unwrap_err().token(),
        hls_cli::PRIVATE_CONFIG_INVALID
    );
}

#[test]
fn private_json_refuses_large_non_object_and_directory_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("private");
    native::create_private_dir_all(&directory).unwrap();
    for (name, bytes) in [
        ("array.json", b"[]".to_vec()),
        ("large.json", vec![b' '; 2 * 1024 * 1024 + 1]),
    ] {
        let path = directory.join(name);
        native::create_private_new(&path)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        assert_eq!(
            hls_cli::private_json(&path).unwrap_err().token(),
            hls_cli::PRIVATE_CONFIG_INVALID
        );
    }
    assert!(hls_cli::private_json(&directory).is_err());
}

#[test]
fn disk_space_uses_the_native_volume_and_preserves_lookup_errors() {
    let temp = tempfile::tempdir().unwrap();
    assert!(drmr_core::source::platform::disk_free(temp.path()).unwrap() > 0);
    assert!(drmr_core::source::platform::disk_free(&temp.path().join("missing")).is_err());
}

#[test]
fn native_file_uris_round_trip_drive_paths_with_escaped_delimiters() {
    let temp = tempfile::tempdir().unwrap();
    let path = native::resolve_path(&temp.path().join("spaces 日本語/#question%.m3u8")).unwrap();
    let uri = drmr_core::source::pypath::as_uri(&path).unwrap();
    assert!(uri.starts_with("file:///"), "{uri}");
    assert!(uri.contains("%23question%25.m3u8"), "{uri}");
    assert!(!uri.contains('\\'));
    let decoded = url::Url::parse(&uri).unwrap().to_file_path().unwrap();
    assert_eq!(native::resolve_path(&decoded).unwrap(), path);
    assert!(drmr_core::source::pypath::as_uri(std::path::Path::new("relative.m3u8")).is_err());
}

#[test]
fn a_windows_local_path_is_analyzed_and_the_frozen_snapshot_reopens() {
    use drmr_core::source::{self, AnalyzeOptions, Direct, FetchPolicy, SourceRequest};
    use std::path::Path;
    use std::sync::Arc;

    let temp = tempfile::tempdir().unwrap();
    let input = temp.path().join("input 日本語");
    std::fs::create_dir(&input).unwrap();
    std::fs::write(
        input.join("master.m3u8"),
        b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nsegment.ts\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    let mut transport_stream = vec![b'x'; 188];
    transport_stream[0] = 0x47;
    std::fs::write(input.join("segment.ts"), transport_stream).unwrap();
    let probe = |_: &Path, _: Option<&Path>| {
        let serde_json::Value::Object(stream) = serde_json::json!({
            "index": 0, "codec_type": "video", "codec_name": "h264",
            "width": 640, "height": 360
        }) else {
            unreachable!()
        };
        Ok(vec![stream])
    };
    let policy = FetchPolicy {
        allowed_local_roots: vec![input.clone()],
        ..FetchPolicy::default()
    };
    let options = AnalyzeOptions {
        on_log: None,
        prober: &probe,
        platform: Arc::new(source::platform::SystemPlatform),
        hooks: &Direct,
    };
    let analysis = source::analyze_hls_with(
        &SourceRequest::new(input.join("master.m3u8").to_string_lossy()),
        "native-local",
        &temp.path().join("output"),
        &policy,
        &options,
    )
    .unwrap();
    assert!(analysis.admitted);
    assert_eq!(analysis.tracks.len(), 1);
    source::verify_source_snapshot(&analysis).unwrap();
    let restored = source::read_analysis(&analysis.snapshot_path).unwrap();
    assert_eq!(restored.snapshot_digest, analysis.snapshot_digest);
    source::verify_source_snapshot(&restored).unwrap();
    private_fs::verify(&native::open_private_read(&analysis.snapshot_path).unwrap()).unwrap();
}
