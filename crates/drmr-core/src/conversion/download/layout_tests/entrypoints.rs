//! Named entrypoints share the source-relative paths and collision rules of their resources.
use super::*;

#[test]
fn source_hls_keeps_safe_root_names_and_nested_playlist_paths() {
    for (remote, local) in [
        ("stream.m3u8?token=entry", "stream.m3u8"),
        ("%E9%9F%B3%E5%A3%B0%20stream.m3u8", "音声 stream.m3u8"),
        ("stream?token=entry", "stream"),
        ("input.m3u8?token=entry", "input.m3u8"),
    ] {
        let parent = crate::native::private_tempdir().unwrap();
        let directory = parent.path().join("source");
        let mut responses = resources(false);
        let master = responses.remove("/vod/master.m3u8").unwrap();
        responses.insert(format!("/vod/{remote}"), master);
        let source = acquire(
            &directory,
            LayoutMode::Source,
            responses,
            &format!("http://example.invalid/vod/{remote}"),
        )
        .unwrap();
        assert_eq!(source.entrypoint_relative(), Path::new(local));
        assert_eq!(
            source.entrypoint(),
            directory.canonicalize().unwrap().join(local)
        );
        if local != "input.m3u8" {
            assert!(!directory.join("input.m3u8").exists());
        }
        let master = std::fs::read_to_string(source.entrypoint()).unwrap();
        assert!(master.contains("video/720p/media.m3u8"));
        let child = std::fs::read_to_string(directory.join("video/720p/media.m3u8")).unwrap();
        assert!(child.contains("\nseg_0.ts\n"));
        assert_eq!(
            std::fs::read(directory.join("video/720p/seg_0.ts")).unwrap(),
            b"retained transport bytes"
        );
        source.check(&Stop::new()).unwrap();
    }
}

#[test]
fn source_hls_uses_the_final_redirected_root_name() {
    let parent = crate::native::private_tempdir().unwrap();
    let directory = parent.path().join("source");
    let mut responses = resources(false);
    let master = responses.remove("/vod/master.m3u8").unwrap();
    responses.insert("/vod/final.m3u8?token=final".into(), master);
    let source = acquire_redirected(
        &directory,
        LayoutMode::Source,
        responses,
        BTreeMap::from([(
            "/old/request.m3u8?token=entry".into(),
            "/vod/final.m3u8?token=final".into(),
        )]),
        "http://example.invalid/old/request.m3u8?token=entry",
    )
    .unwrap();
    assert_eq!(source.entrypoint_relative(), Path::new("final.m3u8"));
    assert!(!directory.join("request.m3u8").exists());
    assert!(!directory.join("input.m3u8").exists());
    assert!(directory.join("video/720p/media.m3u8").is_file());
    source.check(&Stop::new()).unwrap();
}

#[test]
fn source_hls_refuses_children_aliasing_the_root_and_cleans_the_download() {
    for child in ["Stream.m3u8", "stream.m3u8?token=other"] {
        let parent = crate::native::private_tempdir().unwrap();
        let directory = parent.path().join("source");
        let responses = BTreeMap::from([
            (
                "/vod/stream.m3u8?token=entry".into(),
                format!("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\n{child}\n").into_bytes(),
            ),
            (
                format!("/vod/{child}"),
                b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nsegments/seg.ts\n#EXT-X-ENDLIST\n"
                    .to_vec(),
            ),
            ("/vod/segments/seg.ts".into(), b"transport bytes".to_vec()),
        ]);
        assert_eq!(
            acquire(
                &directory,
                LayoutMode::Source,
                responses,
                "http://example.invalid/vod/stream.m3u8?token=entry",
            )
            .unwrap_err(),
            ConversionError("source-layout-collision")
        );
        assert!(!directory.exists());
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }
}

#[test]
fn source_hls_refuses_unsafe_root_names_without_leaving_partial_files() {
    for name in [
        "raw",
        "RAW",
        "%2e%2e",
        "a%2fb.m3u8",
        "a%5cb.m3u8",
        "stream%00.m3u8",
        "CON.m3u8",
        "stream.m3u8.",
        "%GG.m3u8",
    ] {
        let parent = crate::native::private_tempdir().unwrap();
        let directory = parent.path().join("source");
        let mut responses = resources(false);
        let master = responses.remove("/vod/master.m3u8").unwrap();
        responses.insert(format!("/vod/{name}"), master);
        assert!(
            acquire(
                &directory,
                LayoutMode::Source,
                responses,
                &format!("http://example.invalid/vod/{name}"),
            )
            .is_err(),
            "accepted unsafe root name"
        );
        assert!(!directory.exists());
        assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
    }
}

#[test]
fn source_dash_keeps_a_noncanonical_root_name_and_nested_resources() {
    let parent = crate::native::private_tempdir().unwrap();
    let directory = parent.path().join("source");
    let manifest = concat!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT6S">"#,
        r#"<Period><AdaptationSet contentType="video"><Representation id="v" bandwidth="1000" mimeType="video/mp4">"#,
        r#"<SegmentTemplate timescale="1" duration="6" initialization="video/720p/init.mp4" media="video/720p/seg_$Number$.m4s" startNumber="1"/>"#,
        "</Representation></AdaptationSet></Period></MPD>",
    )
    .as_bytes();
    let source = acquire(
        &directory,
        LayoutMode::Source,
        BTreeMap::from([
            (
                "/vod/presentation.xml?token=entry".into(),
                manifest.to_vec(),
            ),
            ("/vod/video/720p/init.mp4".into(), b"init bytes".to_vec()),
            ("/vod/video/720p/seg_1.m4s".into(), b"media bytes".to_vec()),
        ]),
        "http://example.invalid/vod/presentation.xml?token=entry",
    )
    .unwrap();
    assert_eq!(source.entrypoint_relative(), Path::new("presentation.xml"));
    assert!(!directory.join("input.mpd").exists());
    assert_eq!(
        std::fs::read(directory.join("raw/r000000.mpd")).unwrap(),
        manifest
    );
    let projection = std::fs::read_to_string(source.entrypoint()).unwrap();
    assert!(projection.contains("video/720p/init.mp4"));
    assert!(projection.contains("video/720p/seg_1.m4s"));
    assert_eq!(
        std::fs::read(directory.join("video/720p/seg_1.m4s")).unwrap(),
        b"media bytes"
    );
    source.check(&Stop::new()).unwrap();
}
