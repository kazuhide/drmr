use drmr_core::inspection::{Evidence, InputFormat, Protection, TrackKind, inspect};
use std::fs;
use std::path::Path;

fn write(path: &Path, bytes: &str) {
    fs::write(path, bytes).unwrap();
}
fn mpd(body: &str) -> String {
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" xmlns:cenc="urn:mpeg:cenc:2013"><Period>{body}</Period></MPD>"#
    )
}
fn dash(body: &str) -> drmr_core::inspection::Inspection {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("wrong-extension.dat");
    write(&path, &mpd(body));
    inspect(&path).unwrap()
}
#[test]
fn dash_observes_namespaces_inheritance_and_per_representation_mixed_schemes() {
    let result = dash(
        r#"<AdaptationSet mimeType="video/mp4" codecs="avc1.64001f"><ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cenc" cenc:default_KID="00010203-0405-0607-0809-0a0b0c0d0e0f"/><ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/><Representation id="SECRET-LABEL"/><Representation><ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cbcs"/></Representation></AdaptationSet>"#,
    );
    assert_eq!(result.format, InputFormat::Dash);
    assert_eq!(result.evidence, Evidence::Manifest);
    assert!(!result.media_verified);
    assert_eq!(result.tracks.len(), 2);
    assert_eq!(result.tracks[0].kind, TrackKind::Video);
    assert_eq!(result.tracks[0].codec.as_deref(), Some("avc1.64001f"));
    assert_eq!(result.tracks[0].protection, Protection::Cenc);
    assert_eq!(result.tracks[1].protection, Protection::Mixed);
    assert_eq!(result.protection, Protection::Mixed);
    assert_eq!(result.drm_systems, ["Widevine"]);
    assert_eq!(result.tracks[0].key_ids.len(), 1);
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("SECRET-LABEL")
    );
}
#[test]
fn dash_inspection_keeps_conversion_unsupported_codecs_and_structures_observable() {
    let result = dash(
        r#"<AdaptationSet mimeType="video/mp4"><SegmentList/><Representation codecs="avc1.1"/><Representation codecs="av01.0.04M.08"/></AdaptationSet>"#,
    );
    assert_eq!(result.tracks.len(), 2);
    assert_eq!(result.protection, Protection::Clear);
    let result = dash(
        r#"<AdaptationSet><ContentProtection schemeIdUri="urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95"><x:pro xmlns:x="urn:microsoft:playready">RAW-SECRET</x:pro></ContentProtection><Representation/></AdaptationSet>"#,
    );
    assert_eq!(result.protection, Protection::Unknown);
    assert_eq!(result.drm_systems, ["PlayReady"]);
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("RAW-SECRET")
    );
}
#[test]
fn malformed_or_unsupported_dash_never_becomes_clear() {
    for text in [
        "<!DOCTYPE MPD [<!ENTITY x 'x'>]><MPD/>",
        "<MPD xmlns='urn:wrong'><Representation/></MPD>",
        "<MPD><Period>",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("a.mpd");
        write(&path, text);
        assert!(inspect(&path).is_err());
    }
    let result = dash("<AdaptationSet><ContentProtection/><Representation/></AdaptationSet>");
    assert_eq!(result.protection, Protection::Unknown);
    let result = dash("<AdaptationSet/>");
    assert_eq!(result.protection, Protection::Unknown);
}
#[test]
fn dash_xml_node_budget_is_mandatory() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("large.mpd");
    write(&path, &mpd(&"<Representation/>".repeat(25_000)));
    assert!(inspect(&path).is_err());
}
#[test]
fn hls_observes_key_transitions_without_reading_segments_or_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("media.m3u8");
    write(
        &path,
        "#EXTM3U
#EXT-X-KEY:METHOD=AES-128,URI=\"https://SECRET/key\",IV=0x0123456789abcdef0123456789abcdef
#EXTINF:1,
missing.ts
#EXT-X-KEY:METHOD=NONE
#EXTINF:1,
missing2.ts
#EXT-X-ENDLIST
",
    );
    let before = fs::read_dir(tmp.path()).unwrap().count();
    let result = inspect(&path).unwrap();
    assert_eq!(result.protection, Protection::Mixed);
    assert_eq!(result.evidence, Evidence::Manifest);
    assert!(!result.media_verified);
    let json = serde_json::to_string(&result).unwrap();
    for secret in ["SECRET", "0123456789abcdef", "missing.ts"] {
        assert!(!json.contains(secret));
    }
    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), before);
}
#[test]
fn hls_distinguishes_sample_encryption_container_and_drm_declarations() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("media.m3u8");
    for (map, method, expected) in [
        ("", "SAMPLE-AES", Protection::Unknown),
        (
            "#EXT-X-MAP:URI=\"init.mp4\"
",
            "SAMPLE-AES",
            Protection::Unknown,
        ),
        (
            "#EXT-X-MAP:URI=\"init.mp4\"
",
            "SAMPLE-AES-CTR",
            Protection::Unknown,
        ),
    ] {
        write(
            &path,
            &format!(
                "#EXTM3U
{map}#EXT-X-KEY:METHOD={method},URI=\"skd://SECRET\",KEYFORMAT=\"com.apple.streamingkeydelivery\"
#EXTINF:1,
segment
#EXT-X-ENDLIST
"
            ),
        );
        let result = inspect(&path).unwrap();
        assert_eq!(result.protection, expected);
        assert_eq!(result.drm_systems, ["FairPlay"]);
    }
}
#[test]
fn hls_follows_confined_children_and_keeps_remote_references_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root.m3u8");
    write(
        &tmp.path().join("clear.m3u8"),
        "#EXTM3U
#EXTINF:1,
missing.ts
#EXT-X-ENDLIST
",
    );
    write(
        &root,
        "#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=100,CODECS=\"avc1.1\"
clear.m3u8
#EXT-X-STREAM-INF:BANDWIDTH=200
https://SECRET/remote.m3u8
",
    );
    let result = inspect(&root).unwrap();
    assert_eq!(result.protection, Protection::Mixed);
    assert!(
        result
            .tracks
            .iter()
            .any(|t| t.protection == Protection::Unknown)
    );
    assert!(
        result
            .findings
            .iter()
            .any(|f| f.code == "hls-remote-reference")
    );
    assert!(!serde_json::to_string(&result).unwrap().contains("SECRET"));
}
#[test]
fn hls_cycles_traversal_and_symlink_escapes_never_follow_outside_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root.m3u8");
    let master = |reference: &str| {
        format!(
            "#EXTM3U
#EXT-X-STREAM-INF:BANDWIDTH=100
{reference}
"
        )
    };
    for reference in ["root.m3u8", "../outside.m3u8", "/tmp/outside.m3u8"] {
        write(&root, &master(reference));
        let result = inspect(&root).unwrap();
        assert_eq!(result.protection, Protection::Unknown);
        let expected = if reference == "root.m3u8" {
            "hls-reference-cycle"
        } else {
            "hls-reference-outside-root"
        };
        assert!(
            result
                .findings
                .iter()
                .any(|finding| finding.code == expected)
        );
    }
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        write(
            &outside.path().join("secret.m3u8"),
            "#EXTM3U
#EXTINF:1,
x
#EXT-X-ENDLIST
",
        );
        std::os::unix::fs::symlink(
            outside.path().join("secret.m3u8"),
            tmp.path().join("escape.m3u8"),
        )
        .unwrap();
        write(&root, &master("escape.m3u8"));
        let result = inspect(&root).unwrap();
        assert_eq!(result.protection, Protection::Unknown);
    }
}
#[test]
fn hls_duplicate_reference_budget_is_not_bypassed_by_caching() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("root.m3u8");
    write(
        &tmp.path().join("child.m3u8"),
        "#EXTM3U
#EXTINF:1,
x
#EXT-X-ENDLIST
",
    );
    write(
        &root,
        &format!(
            "#EXTM3U
{}",
            "#EXT-X-STREAM-INF:BANDWIDTH=1
child.m3u8
"
            .repeat(150)
        ),
    );
    assert!(inspect(&root).is_err());
}
#[test]
fn malformed_hls_key_or_mixed_playlist_never_is_clear() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.m3u8");
    for body in [
        "#EXT-X-KEY:URI=\"SECRET\"
#EXTINF:1,
x
",
        "#EXT-X-KEY:METHOD=AES-128,METHOD=NONE
#EXTINF:1,
x
",
        "#EXT-X-STREAM-INF:BANDWIDTH=1
child
#EXTINF:1,
x
",
    ] {
        write(
            &path,
            &format!(
                "#EXTM3U
{body}"
            ),
        );
        if let Ok(result) = inspect(&path) {
            assert_ne!(result.protection, Protection::Clear);
        }
    }
}

#[test]
fn invalid_hls_durations_and_unfinished_segments_are_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.m3u8");
    for body in [
        "#EXTINF:not-a-number,\nx\n",
        "#EXTINF:NaN,\nx\n",
        "#EXTINF:-1,\nx\n",
        "#EXTINF:1,\n",
    ] {
        write(&path, &format!("#EXTM3U\n{body}"));
        assert!(inspect(&path).is_err(), "{body}");
    }
}

#[test]
fn encrypted_initialization_section_is_not_lost_after_key_reset() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.m3u8");
    write(
        &path,
        "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"SECRET\",IV=0x01\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXT-X-KEY:METHOD=NONE\n#EXTINF:1,\nx\n",
    );
    assert_eq!(inspect(&path).unwrap().protection, Protection::Mixed);
}

#[test]
fn unsupported_dash_hierarchy_is_not_reported_as_clear() {
    let result = dash(
        r#"<foreign:AdaptationSet xmlns:foreign="urn:unknown"><Representation/></foreign:AdaptationSet>"#,
    );
    assert_eq!(result.protection, Protection::Unknown);
}

#[test]
fn hls_invalid_required_attribute_values_never_report_clear_or_known_encryption() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.m3u8");
    write(&tmp.path().join("child.m3u8"), "#EXTM3U\n#EXTINF:1,\nx\n");
    for body in [
        "#EXT-X-STREAM-INF:CODECS=\"avc1.1\"\nchild.m3u8\n",
        "#EXT-X-STREAM-INF:BANDWIDTH=0\nchild.m3u8\n",
        "#EXT-X-KEY:METHOD=AES-128,URI=\"\"\n#EXTINF:1,\nx\n",
    ] {
        write(&path, &format!("#EXTM3U\n{body}"));
        if let Ok(result) = inspect(&path) {
            assert_eq!(result.protection, Protection::Unknown);
        }
    }
}

#[test]
fn hls_non_regular_child_is_not_opened() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.m3u8");
    fs::create_dir(tmp.path().join("directory.m3u8")).unwrap();
    write(
        &path,
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\ndirectory.m3u8\n",
    );
    let result = inspect(&path).unwrap();
    assert_eq!(result.protection, Protection::Unknown);
    assert!(
        result
            .findings
            .iter()
            .any(|finding| finding.code == "hls-reference-unreadable")
    );
}

#[test]
fn unsupported_partial_segments_and_nested_protection_are_not_silently_clear() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("a.m3u8");
    write(
        &path,
        "#EXTM3U\n#EXTINF:1,\nclear.ts\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"SECRET\"\n#EXT-X-PART:DURATION=0.5,URI=\"SECRET-part\"\n",
    );
    let result = inspect(&path).unwrap();
    assert_eq!(result.protection, Protection::Mixed);
    assert!(
        result
            .findings
            .iter()
            .any(|finding| finding.code == "hls-partial-segments-unverified")
    );
    let result = dash(
        r#"<AdaptationSet><Representation><SubRepresentation><ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cenc"/></SubRepresentation></Representation></AdaptationSet>"#,
    );
    assert_eq!(result.protection, Protection::Unknown);
}
