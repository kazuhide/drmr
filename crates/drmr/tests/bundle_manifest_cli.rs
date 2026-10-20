//! Manifest-only routing must not apply the media inspector's graph limits.
#![cfg(unix)]

use serde_json::{Value, json};
use std::{fmt::Write as _, os::unix::fs::PermissionsExt as _, process::Command};

#[test]
fn manifest_only_checks_all_33_playlists_without_media_or_keys() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("playlist.data");
    let marker = root.path().join("validator-calls");
    let mut master = "#EXTM3U\n".to_owned();
    for index in 0..32 {
        let name = format!("variant-{index:02}.list");
        write!(master, "#EXT-X-STREAM-INF:BANDWIDTH=64000\n{name}\n").unwrap();
        std::fs::write(
            root.path().join(name),
            concat!(
                "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:2\n",
                "#EXT-X-MAP:URI=\"missing-init.mp4\"\n",
                "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://not-fetched\",",
                "KEYFORMAT=\"com.apple.streamingkeydelivery\"\n",
                "#EXTINF:2.0,\nmissing-segment.m4s\n#EXT-X-ENDLIST\n",
            ),
        )
        .unwrap();
    }
    std::fs::write(&input, &master).unwrap();
    let media = json!({
        "playlistKind":"media", "fetchCount":1, "parsedSegmentsCount":1,
        "processedSegmentsCount":0, "hasEndTag":true,
        "discontinuities":[{"segments":[{"processed":false}]}]
    });
    let mut child = media.clone();
    child["dataVersion"] = json!(1.2);
    child["validatorName"] = "mediastreamvalidator".into();
    child["validatorVersion"] = "fixture".into();
    child["url"] = "%s".into();
    let parent = json!({
        "dataVersion":1.2, "validatorName":"mediastreamvalidator",
        "validatorVersion":"fixture", "url":"%s",
        "playlistKind":"multivariant", "variants":vec![media;32]
    });
    let script = root.path().join("validator");
    let quoted_marker = marker.to_str().unwrap().replace('\'', "'\\''");
    std::fs::write(&script, format!(
        "#!/bin/sh\nprintf '.\\n' >> '{quoted_marker}'\ncase \"$6\" in\n*/playlist.data) printf '{parent}' \"$6\";;\n*) printf '{child}' \"$6\";;\nesac\n"
    )).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args([
            "verify",
            input.to_str().unwrap(),
            "--manifest-only",
            "--external-validator",
            "--hls-validator-bin",
            script.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["schema_version"], 2);
    assert_eq!(report["manifest_count"], 33);
    assert_eq!(report["native_manifest"]["document_count"], 33);
    assert_eq!(report["native_manifest"]["errors"], 0);
    assert_eq!(report["external_requested"], true);
    assert_eq!(
        report["external_manifest"]["checks"][0]["checked_manifest_count"],
        33
    );
    assert_eq!(report["scope"], "manifest-only");
    assert_eq!(report["media_verified"], false);
    assert_eq!(report["license_playback_verified"], false);
    assert_eq!(std::fs::read_to_string(marker).unwrap().lines().count(), 33);
    assert_eq!(std::fs::read_to_string(&input).unwrap(), master);
    assert!(!root.path().join("missing-init.mp4").exists());
    assert!(!root.path().join("missing-segment.m4s").exists());
}
