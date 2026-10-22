//! Windows private configuration, local paths, and byte-bound media graphs.
#![cfg(windows)]
use drmr_core::{
    concurrency::Stop,
    conversion::{FrozenSource, key_file},
    native,
};
use std::io::Write as _;

#[test]
fn key_configuration_accepts_a_user_only_file_and_refuses_inherited_access() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("private 日本語");
    native::create_private_dir_all(&directory).unwrap();
    let path = directory.join("keys.json");
    native::create_private_new(&path)
        .unwrap()
        .write_all(br#"{"schema_version":1}"#)
        .unwrap();
    key_file::read(&path).unwrap();
    let inherited = temp.path().join("inherited.json");
    std::fs::write(&inherited, br#"{"schema_version":1}"#).unwrap();
    assert_eq!(
        key_file::read(&inherited).unwrap_err().0,
        "key-file-invalid"
    );
    assert!(native::open_private_read(&inherited).is_err());
}
#[test]
fn key_configuration_refuses_large_non_object_and_directory_inputs() {
    let temp = native::private_tempdir().unwrap();
    for (name, bytes) in [
        ("array.json", b"[]".to_vec()),
        ("large.json", vec![b' '; 65537]),
    ] {
        let path = temp.path().join(name);
        native::create_private_new(&path)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        assert_eq!(key_file::read(&path).unwrap_err().0, "key-file-invalid");
    }
    assert!(key_file::read(temp.path()).is_err());
}
#[test]
fn a_windows_local_graph_reopens_only_when_its_original_bytes_match() {
    let temp = native::private_tempdir().unwrap();
    let path = temp.path().join("日本語 clip.mp4");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb/clip.mp4");
    std::fs::copy(fixture, &path).unwrap();
    let stop = Stop::new();
    let frozen = FrozenSource::capture(&path, &stop).unwrap();
    let inventory = frozen.inventory().clone();
    let reopened = FrozenSource::from_inventory(&inventory, &stop).unwrap();
    assert_eq!(
        std::fs::read(frozen.entrypoint()).unwrap(),
        std::fs::read(reopened.entrypoint()).unwrap()
    );
    private_fs::verify(&native::open_private_read(reopened.entrypoint()).unwrap()).unwrap();
    std::fs::write(&path, b"changed media").unwrap();
    assert!(FrozenSource::from_inventory(&inventory, &stop).is_err());
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
fn native_media_paths_resolve_and_keep_their_filename_components() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("missing/manifest.mpd");
    let resolved = drmr_core::pycompat::resolve(&path).unwrap();
    assert_eq!(resolved, native::resolve_path(&path).unwrap());
    let pure = drmr_core::pycompat::PurePosixPath::from_path(&resolved);
    assert_eq!(pure.name(), "manifest.mpd");
    assert_eq!(pure.parent().name(), "missing");
    assert_eq!(native::resolve_path(&pure.to_path_buf()).unwrap(), resolved);
}
