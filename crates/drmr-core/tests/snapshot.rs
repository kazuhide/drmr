//! The snapshot digest and verification against a record the Python fixture generator wrote.

use drmr_core::canonical::Dumps;
use drmr_core::snapshot::{
    SnapshotError, analysis_id, safe_local, sha256_hex, snapshot_digest, verify_files,
    verify_record,
};
use serde_json::{Value, json};
use std::path::PathBuf;

fn sample() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/analysis.sample.json")
}

fn sample_value() -> Value {
    serde_json::from_slice(&std::fs::read(sample()).unwrap()).unwrap()
}

#[test]
fn the_sample_record_verifies_and_its_ids_derive_from_the_digest() {
    let report = verify_record(&sample()).unwrap();
    assert_eq!(report.recorded_digest, report.measured_digest);
    assert_eq!(report.analysis_id, analysis_id(&report.measured_digest));
    // The digest the Python generator computed with json.dumps(sort_keys=True, separators=(",", ":")).
    assert_eq!(
        report.measured_digest,
        "e63d73664e10730c148d26c0eaf1f16b74b22494d2e374ff65712f39a0a50154"
    );
    assert_eq!(report.analysis_id, "hls-e63d73664e10730c148d26c0");
    assert_eq!(report.asset_id, "sample");
    assert_eq!(
        (report.resources, report.tracks, report.files_verified),
        (2, 1, None)
    );
}

#[test]
fn a_changed_record_is_snapshot_changed_and_a_malformed_one_snapshot_invalid() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("analysis.json");
    let mut value = sample_value();
    value["tracks"][0]["segments"][0]["duration"] = json!(5.999);
    std::fs::write(&path, Dumps::SORTED.dumps(&value)).unwrap();
    assert_eq!(verify_record(&path), Err(SnapshotError::Changed));

    let mut value = sample_value();
    value["analysis_id"] = json!("hls-000000000000000000000000");
    std::fs::write(&path, Dumps::SORTED.dumps(&value)).unwrap();
    let report = verify_record(&path).unwrap();
    assert_eq!(
        report.analysis_id, "hls-000000000000000000000000",
        "the id is reported as recorded, not checked against the digest (Python does not; R3)"
    );
    assert_eq!(report.recorded_digest, report.measured_digest);

    std::fs::write(&path, r#"{"asset_id": "x", "resources": [], "tracks": []}"#).unwrap();
    assert_eq!(
        verify_record(&path),
        Err(SnapshotError::Invalid),
        "no digest"
    );

    let mut value = sample_value();
    value["snapshot_digest"] = json!(42);
    std::fs::write(&path, Dumps::SORTED.dumps(&value)).unwrap();
    assert_eq!(
        verify_record(&path),
        Err(SnapshotError::Changed),
        "a digest that is not a string compares unequal, as Python's `!=` does"
    );

    let mut value = sample_value();
    value["schema_version"] = json!("2");
    std::fs::write(&path, Dumps::SORTED.dumps(&value)).unwrap();
    assert_eq!(
        verify_record(&path),
        Err(SnapshotError::Invalid),
        "an unknown schema_version is snapshot-invalid (hls_source.read_analysis)"
    );
    std::fs::write(&path, "not json").unwrap();
    assert_eq!(verify_record(&path), Err(SnapshotError::Invalid));
    assert_eq!(
        verify_record(&dir.path().join("missing.json")),
        Err(SnapshotError::Invalid)
    );
    assert_eq!(SnapshotError::Changed.token(), "snapshot-changed");
    assert_eq!(SnapshotError::Invalid.to_string(), "snapshot-invalid");
}

#[test]
fn files_are_rehashed_inside_the_snapshot_directory() {
    let dir = tempfile::tempdir().unwrap();
    let playlist = b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nr000001.ts\n#EXT-X-ENDLIST\n";
    let segment = b"\x47\x00\x11\x10segment";
    std::fs::write(dir.path().join("r000000.m3u8"), playlist).unwrap();
    std::fs::write(dir.path().join("r000001.ts"), segment).unwrap();
    let mut private = json!({
        "schema_version": "1", "asset_id": "local",
        "resources": [
            {"byte_range": null, "id": "r000000", "kind": "playlist", "path": "r000000.m3u8",
             "sha256": sha256_hex(playlist), "url": "https://media.example.invalid/x.m3u8"},
            {"byte_range": null, "id": "r000001", "kind": "segment", "path": "r000001.ts",
             "sha256": sha256_hex(segment), "url": "https://media.example.invalid/r000001.ts"}],
        "tracks": []});
    let digest = snapshot_digest(&private);
    private["snapshot_digest"] = json!(digest);
    private["analysis_id"] = json!(analysis_id(&digest));
    std::fs::write(
        dir.path().join("analysis.json"),
        Dumps::SORTED.dumps(&private),
    )
    .unwrap();

    let report = verify_files(dir.path()).unwrap();
    assert_eq!(report.files_verified, Some(2));
    assert_eq!(report.asset_id, "local");

    std::fs::write(dir.path().join("r000001.ts"), b"different").unwrap();
    assert_eq!(verify_files(dir.path()), Err(SnapshotError::Changed));
    std::fs::remove_file(dir.path().join("r000001.ts")).unwrap();
    assert_eq!(verify_files(dir.path()), Err(SnapshotError::Invalid));
    assert!(
        verify_record(dir.path()).is_ok(),
        "the record itself did not change"
    );

    for bad in ["", "../x", "/etc/passwd", "a/../b"] {
        assert_eq!(safe_local(dir.path(), bad), None, "{bad}");
    }
    // `.` components and a trailing `/` are dropped, as `pathlib` joins them.
    for good in [
        "sub/r.ts",
        "./sub/r.ts",
        "sub/./r.ts",
        "sub/r.ts/",
        "sub//r.ts/.",
    ] {
        assert_eq!(
            safe_local(dir.path(), good),
            Some(dir.path().join("sub/r.ts")),
            "{good}"
        );
    }
}

/// A record whose one resource is `name` with the given bytes' digest, written to `dir`; with
/// `segment`, one track whose single segment file is that path.
fn write_record(dir: &std::path::Path, name: &str, content: &[u8], segment: Option<&str>) {
    let tracks = segment.map_or_else(Vec::new, |path| {
        vec![json!({"id": "v000", "init_path": null,
                    "segments": [{"index": 0, "path": path, "sha256": sha256_hex(content)}]})]
    });
    let mut value = json!({
        "schema_version": "1",
        "asset_id": "link",
        "resources": [{"path": name, "sha256": sha256_hex(content)}],
        "tracks": tracks
    });
    let digest = snapshot_digest(&value);
    value["analysis_id"] = json!(analysis_id(&digest));
    value["snapshot_digest"] = json!(digest);
    std::fs::write(dir.join("analysis.json"), Dumps::SORTED.dumps(&value)).unwrap();
}

#[cfg(unix)]
#[test]
fn a_resource_that_resolves_outside_the_snapshot_directory_is_snapshot_invalid() {
    // `safe_local` is lexical; the real file is resolved like `source::_safe_local` does
    // (symlinks followed, then confined, and a regular file).
    let outside = tempfile::tempdir().unwrap();
    let content = b"segment bytes";
    std::fs::write(outside.path().join("real.ts"), content).unwrap();
    let dir = tempfile::tempdir().unwrap();
    write_record(dir.path(), "r000001.ts", content, None);
    std::os::unix::fs::symlink(
        outside.path().join("real.ts"),
        dir.path().join("r000001.ts"),
    )
    .unwrap();
    assert!(
        verify_record(dir.path()).is_ok(),
        "the record itself is sound"
    );
    assert_eq!(
        verify_files(dir.path()),
        Err(SnapshotError::Invalid),
        "a link out of the directory is refused even when the bytes match"
    );

    std::fs::remove_file(dir.path().join("r000001.ts")).unwrap();
    std::fs::create_dir(dir.path().join("r000001.ts")).unwrap();
    assert_eq!(
        verify_files(dir.path()),
        Err(SnapshotError::Invalid),
        "a directory is not a resource file"
    );

    std::fs::remove_dir(dir.path().join("r000001.ts")).unwrap();
    std::fs::write(dir.path().join("r000001.ts"), content).unwrap();
    assert_eq!(verify_files(dir.path()).unwrap().files_verified, Some(1));
}

#[test]
fn track_files_must_be_present_inside_the_directory_like_read_analysis_requires() {
    let dir = tempfile::tempdir().unwrap();
    let content = b"segment bytes";
    std::fs::write(dir.path().join("r000001.ts"), content).unwrap();
    write_record(dir.path(), "r000001.ts", content, Some("v000/s000000.ts"));
    assert!(
        verify_record(dir.path()).is_ok(),
        "the record alone is sound"
    );
    assert_eq!(
        verify_files(dir.path()),
        Err(SnapshotError::Invalid),
        "a missing track file is snapshot-invalid (hls_source.read_analysis)"
    );
    write_record(dir.path(), "r000001.ts", content, Some("../s000000.ts"));
    assert_eq!(verify_files(dir.path()), Err(SnapshotError::Invalid));
    write_record(dir.path(), "r000001.ts", content, Some("v000/s000000.ts"));
    std::fs::create_dir(dir.path().join("v000")).unwrap();
    std::fs::write(dir.path().join("v000/s000000.ts"), content).unwrap();
    assert_eq!(
        verify_files(dir.path()).unwrap().files_verified,
        Some(1),
        "resources are hashed; track files are required to exist"
    );
}
