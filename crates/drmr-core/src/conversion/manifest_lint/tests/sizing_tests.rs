use super::*;

#[test]
fn preflight_discovery_keeps_its_reservation_until_the_snapshot_is_consumed() {
    let root = crate::native::private_tempdir().unwrap();
    let input = input(root.path());
    let limiter = Arc::new(Limiter::new(1, 544 * 1024 * 1024));
    let command = converter(Arc::clone(&limiter));
    let captured = crate::conversion::manifest_lint::snapshot::Snapshot::capture_metadata(
        &command,
        &input,
        Format::Hls,
    )
    .unwrap();
    assert_eq!(
        limiter
            .acquire(
                Claim {
                    cpu_threads: 1,
                    memory_bytes: 1
                },
                None,
                Some(Duration::from_millis(20)),
            )
            .unwrap_err(),
        ReservationError::Timeout
    );
    drop(captured);
    drop(
        limiter
            .acquire(
                Claim {
                    cpu_threads: 1,
                    memory_bytes: 544 * 1024 * 1024,
                },
                None,
                Some(Duration::from_secs(1)),
            )
            .unwrap(),
    );
}

fn control_graph(root: &Path) -> std::path::PathBuf {
    let input = root.join("master.m3u8");
    std::fs::create_dir(root.join("nested")).unwrap();
    std::fs::write(
        &input,
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100000\nnested/variant\n",
    )
    .unwrap();
    std::fs::write(
        root.join("nested/variant"),
        media().replace(
            "#EXT-X-MAP:",
            "#EXT-X-KEY:METHOD=AES-128,URI=\"https://manifest-key.invalid/private\"\n#EXT-X-MAP:",
        ),
    )
    .unwrap();
    input
}

#[test]
fn preflight_seals_every_control_manifest_without_opening_media_or_key_addresses() {
    let root = crate::native::private_tempdir().unwrap();
    let input = control_graph(root.path());
    let command = converter(Arc::new(Limiter::new(1, 544 * 1024 * 1024)));
    let first = command.manifest_demand(&input).unwrap().unwrap();
    assert!(first.minimum_bytes > 0 && first.metadata_bytes > 0);
    assert!(!root.path().join("nested/absent-init.mp4").exists());
    assert!(!root.path().join("nested/absent-segment.m4s").exists());
    let child = root.path().join("nested/variant");
    let mut changed = std::fs::read_to_string(&child).unwrap();
    changed.push_str("# An ordinary comment changes the bound source bytes.\n");
    std::fs::write(child, changed).unwrap();
    let second = command.manifest_demand(&input).unwrap().unwrap();
    assert_ne!(first.seal, second.seal);
    assert!(second.minimum_bytes > first.minimum_bytes);
}

#[test]
fn frozen_control_graphs_match_the_raw_seal_and_new_child_references_refuse() {
    let root = crate::native::private_tempdir().unwrap();
    let input = control_graph(root.path());
    for name in ["absent-init.mp4", "absent-segment.m4s"] {
        std::fs::write(
            root.path().join("nested").join(name),
            b"bounded media fixture",
        )
        .unwrap();
    }
    let command = converter(Arc::new(Limiter::new(1, 544 * 1024 * 1024)));
    let demand = command.manifest_demand(&input).unwrap();
    let original = command.capture_source(&input).unwrap();
    command
        .bind_manifest_demand(&original, demand)
        .unwrap()
        .unwrap()
        .check(&command)
        .unwrap();
    let new_child = root.path().join("nested/replaced");
    std::fs::write(&new_child, media()).unwrap();
    let changed = std::fs::read_to_string(&input)
        .unwrap()
        .replace("nested/variant", "nested/replaced");
    std::fs::write(&input, changed).unwrap();
    let replaced = command.capture_source(&input).unwrap();
    assert_eq!(
        command
            .bind_manifest_demand(&replaced, demand)
            .err()
            .unwrap(),
        ConversionError("verification-output-changed"),
    );
}

#[test]
fn dash_sizing_reads_only_the_control_manifest_and_binds_its_private_copy() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("manifest.mpd");
    let document = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" profiles="urn:mpeg:dash:profile:isoff-live:2011" minBufferTime="PT1S" mediaPresentationDuration="PT8S"><Period><AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f"><SegmentTemplate timescale="1000" media="seg-$Number$.m4s" initialization="init.mp4"><SegmentTimeline><S t="0" d="2000" r="3"/></SegmentTimeline></SegmentTemplate><Representation id="v1" bandwidth="1000000"/></AdaptationSet></Period></MPD>"#;
    std::fs::write(&input, document).unwrap();
    let command = converter(Arc::new(Limiter::new(1, 544 * 1024 * 1024)));
    let demand = command.manifest_demand(&input).unwrap();
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    for name in [
        "init.mp4",
        "seg-1.m4s",
        "seg-2.m4s",
        "seg-3.m4s",
        "seg-4.m4s",
    ] {
        std::fs::write(root.path().join(name), b"bounded media fixture").unwrap();
    }
    let original = command.capture_source(&input).unwrap();
    command
        .bind_manifest_demand(&original, demand)
        .unwrap()
        .unwrap()
        .check(&command)
        .unwrap();
    let changed = document.replace("bandwidth=\"1000000\"", "bandwidth=\"2000000\"");
    std::fs::write(&input, changed).unwrap();
    let replaced = command.capture_source(&input).unwrap();
    assert_eq!(
        command
            .bind_manifest_demand(&replaced, demand)
            .err()
            .unwrap(),
        ConversionError("verification-output-changed"),
    );
}
