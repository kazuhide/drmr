//! Snapshot track identifiers must remain safe output path components.

use drmr_core::canonical::Dumps;
use drmr_core::snapshot::{analysis_id, sha256_file, snapshot_digest};
use drmr_core::source::{Failure, read_analysis};
use serde_json::{Value, json};

fn write_snapshot(path: &std::path::Path, mut data: Value) {
    let map = data.as_object_mut().unwrap();
    map.shift_remove("snapshot_digest");
    map.shift_remove("analysis_id");
    let digest = snapshot_digest(&data);
    data["snapshot_digest"] = json!(digest);
    data["analysis_id"] = json!(analysis_id(&digest));
    std::fs::write(path, Dumps::SORTED.dumps(&data)).unwrap();
}

#[test]
fn imported_snapshots_reject_unsafe_track_identifiers() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("analysis.json");
    let mut data: Value =
        serde_json::from_str(include_str!("fixtures/hls_source/analysis/cue.json")).unwrap();
    for resource in data["resources"].as_array_mut().unwrap() {
        let file = tmp.path().join(resource["path"].as_str().unwrap());
        std::fs::write(&file, b"synthetic snapshot resource").unwrap();
        resource["sha256"] = json!(sha256_file(&file).unwrap());
    }
    for segment in data["tracks"][0]["segments"].as_array_mut().unwrap() {
        let file = tmp.path().join(segment["path"].as_str().unwrap());
        segment["sha256"] = json!(sha256_file(&file).unwrap());
    }
    write_snapshot(&path, data.clone());
    read_analysis(&path).expect("the unchanged synthetic snapshot is valid");
    for id in [
        "",
        ".",
        "..",
        "/tmp/escaped",
        "nested/track",
        "nested\\track",
        "C:\\escaped",
        "C:escaped",
        "\\\\server\\share",
        "bad\0id",
        "bad\nid",
    ] {
        let mut changed = data.clone();
        changed["tracks"][0]["id"] = json!(id);
        write_snapshot(&path, changed);
        let error = read_analysis(&path).unwrap_err();
        assert!(
            matches!(&error, Failure::Source(token) if token.token() == "snapshot-invalid"),
            "{id:?}: {error}"
        );
    }
}
