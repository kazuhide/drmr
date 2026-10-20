//! Saved conversion sources bind every referenced byte without adopting cleanup ownership.
use drmr_core::{
    concurrency::Stop,
    conversion::{FrozenSource, SourceInventory},
};
use std::path::{Path, PathBuf};

fn hls(root: &Path) -> PathBuf {
    std::fs::create_dir(root.join("media")).unwrap();
    std::fs::write(root.join("media/a.ts"), b"first original segment").unwrap();
    std::fs::write(root.join("media/b.ts"), b"second original segment").unwrap();
    let input = root.join("input.m3u8");
    std::fs::write(&input, "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nmedia/a.ts\n#EXTINF:2,\nmedia/b.ts\n#EXT-X-ENDLIST\n").unwrap();
    input
}

#[test]
fn saved_graph_reopens_exact_private_bytes_and_excludes_unrelated_files() {
    let root = tempfile::tempdir().unwrap();
    let input = hls(root.path());
    std::fs::write(
        root.path().join("operator-private.json"),
        b"not source media",
    )
    .unwrap();
    let source = FrozenSource::capture(&input, &Stop::new()).unwrap();
    let inventory = source.inventory().clone();
    assert_eq!(inventory.files.len(), 3);
    assert!(
        inventory
            .files
            .iter()
            .all(|file| file.path != Path::new("operator-private.json"))
    );
    assert_ne!(source.entrypoint(), input);
    let encoded = serde_json::to_vec(&inventory).unwrap();
    let decoded: SourceInventory = serde_json::from_slice(&encoded).unwrap();
    drop(source);
    let reopened = FrozenSource::from_inventory(&decoded, &Stop::new()).unwrap();
    assert_eq!(
        std::fs::read(reopened.entrypoint()).unwrap(),
        std::fs::read(&input).unwrap()
    );
    assert_eq!(reopened.inventory(), &inventory);
    reopened.check_original().unwrap();
    drop(reopened);
    assert!(input.exists());
    assert_eq!(
        std::fs::read(root.path().join("operator-private.json")).unwrap(),
        b"not source media"
    );
}

#[test]
fn each_original_reference_is_required_and_changed_bytes_are_refused() {
    for name in ["input.m3u8", "media/a.ts", "media/b.ts"] {
        let root = tempfile::tempdir().unwrap();
        let input = hls(root.path());
        let source = FrozenSource::capture(&input, &Stop::new()).unwrap();
        let inventory = source.inventory().clone();
        std::fs::write(root.path().join(name), b"changed original").unwrap();
        assert!(FrozenSource::from_inventory(&inventory, &Stop::new()).is_err());
        assert!(source.check_original().is_err());
        assert!(root.path().join(name).exists());
    }
}

#[test]
fn unknown_fields_and_cancelled_capture_are_refused_without_source_changes() {
    let root = tempfile::tempdir().unwrap();
    let input = hls(root.path());
    let source = FrozenSource::capture(&input, &Stop::new()).unwrap();
    let mut value = serde_json::to_value(source.inventory()).unwrap();
    value["executable"] = "untrusted".into();
    assert!(serde_json::from_value::<SourceInventory>(value).is_err());
    let stop = Stop::new();
    stop.set();
    assert!(FrozenSource::capture(&input, &stop).is_err());
    assert!(FrozenSource::from_inventory(source.inventory(), &stop).is_err());
    assert!(input.exists());
}

#[test]
fn local_mp4_and_finite_dash_templates_capture_only_actual_references() {
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("source.mp4");
    std::fs::write(&input, b"\0\0\0\x18ftypisomprivate source bytes").unwrap();
    let mp4 = FrozenSource::capture(&input, &Stop::new()).unwrap();
    assert_eq!(mp4.inventory().files.len(), 1);
    assert_eq!(
        std::fs::read(mp4.entrypoint()).unwrap(),
        std::fs::read(&input).unwrap()
    );

    std::fs::create_dir(root.path().join("video")).unwrap();
    for file in ["init.mp4", "s-00007.m4s", "s-00008.m4s"] {
        std::fs::write(root.path().join("video").join(file), file.as_bytes()).unwrap();
    }
    let manifest = root.path().join("manifest.mpd");
    std::fs::write(&manifest, r#"<MPD type="static" mediaPresentationDuration="PT4S"><Period><AdaptationSet><Representation id="v"><BaseURL>video/</BaseURL><SegmentTemplate timescale="1" startNumber="7" initialization="init.mp4" media="s-$Number%05d$.m4s"><SegmentTimeline><S d="2" r="1"/></SegmentTimeline></SegmentTemplate></Representation></AdaptationSet></Period></MPD>"#).unwrap();
    let dash = FrozenSource::capture(&manifest, &Stop::new()).unwrap();
    assert_eq!(dash.inventory().files.len(), 4);
    let saved = dash.inventory().clone();
    let reopened = FrozenSource::from_inventory(&saved, &Stop::new()).unwrap();
    assert_eq!(reopened.inventory(), &saved);
    for file in &saved.files {
        let original = std::fs::read(saved.root.join(&file.path)).unwrap();
        let frozen =
            std::fs::read(reopened.entrypoint().parent().unwrap().join(&file.path)).unwrap();
        assert_eq!(original, frozen);
    }
}

#[test]
fn master_playlists_keep_child_relative_references_and_decode_paths_once() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("video")).unwrap();
    std::fs::write(root.path().join("video/segment a.ts"), b"source media").unwrap();
    std::fs::write(
        root.path().join("video/stream.m3u8"),
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nsegment%20a.ts\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    let input = root.path().join("master.m3u8");
    std::fs::write(
        &input,
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nvideo/stream.m3u8\n",
    )
    .unwrap();
    let source = FrozenSource::capture(&input, &Stop::new()).unwrap();
    assert_eq!(source.inventory().files.len(), 3);
    assert!(
        source
            .inventory()
            .files
            .iter()
            .any(|file| file.path == Path::new("video/segment a.ts"))
    );
    FrozenSource::from_inventory(source.inventory(), &Stop::new()).unwrap();
}

#[test]
fn escaping_remote_and_unknown_resource_references_fail_without_removing_originals() {
    for reference in [
        "../outside.ts",
        "%2e%2e/outside.ts",
        "https://example.invalid/media.ts",
        "file:///outside.ts",
        "segment.ts?query=value",
        "segment%2fts/../../outside",
    ] {
        let root = tempfile::tempdir().unwrap();
        let input = hls(root.path());
        std::fs::write(
            &input,
            format!("#EXTM3U\n#EXTINF:2,\n{reference}\n#EXT-X-ENDLIST\n"),
        )
        .unwrap();
        assert!(
            FrozenSource::capture(&input, &Stop::new()).is_err(),
            "{reference}"
        );
        assert!(input.exists());
        assert!(root.path().join("media/a.ts").exists());
    }
}

#[test]
#[cfg(unix)]
fn source_links_and_directory_links_cannot_redirect_the_copied_graph() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let input = hls(root.path());
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("a.ts"), b"private foreign media").unwrap();
    std::fs::remove_file(root.path().join("media/a.ts")).unwrap();
    symlink(outside.path().join("a.ts"), root.path().join("media/a.ts")).unwrap();
    assert!(FrozenSource::capture(&input, &Stop::new()).is_err());
    std::fs::remove_file(root.path().join("media/a.ts")).unwrap();
    std::fs::hard_link(outside.path().join("a.ts"), root.path().join("media/a.ts")).unwrap();
    assert!(FrozenSource::capture(&input, &Stop::new()).is_err());
    std::fs::remove_file(root.path().join("media/a.ts")).unwrap();
    std::fs::remove_file(root.path().join("media/b.ts")).unwrap();
    std::fs::remove_dir(root.path().join("media")).unwrap();
    symlink(outside.path(), root.path().join("media")).unwrap();
    assert!(FrozenSource::capture(&input, &Stop::new()).is_err());
    assert_eq!(
        std::fs::read(outside.path().join("a.ts")).unwrap(),
        b"private foreign media"
    );
}
