//! Response content selects manifests even when effective entrypoints are directory URLs.
use super::*;

fn responses(base: &str, dash: bool) -> BTreeMap<String, Vec<u8>> {
    let (manifest, files) = if dash {
        (
            br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT6S"><Period><AdaptationSet contentType="video"><Representation id="v" bandwidth="1000" mimeType="video/mp4"><SegmentTemplate timescale="1" duration="6" initialization="video/720p/init.mp4" media="video/720p/seg_$Number$.m4s" startNumber="1"/></Representation></AdaptationSet></Period></MPD>"#.to_vec(),
            vec![("video/720p/init.mp4", b"initialization bytes".to_vec()), ("video/720p/seg_1.m4s", b"media bytes".to_vec())],
        )
    } else {
        (
            b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nvideo/720p/media.m3u8\n".to_vec(),
            vec![("video/720p/media.m3u8", b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nseg_0.ts\n#EXT-X-ENDLIST\n".to_vec()), ("video/720p/seg_0.ts", b"media bytes".to_vec())],
        )
    };
    let mut values = BTreeMap::from([(base.into(), manifest)]);
    let directory = base.split('?').next().unwrap();
    for (relative, bytes) in files {
        values.insert(format!("{directory}{relative}"), bytes);
    }
    values
}

#[test]
fn hls_and_dash_directory_entrypoints_retain_nested_resource_suffixes() {
    for dash in [false, true] {
        for base in ["/", "/vod/", "/vod/?token=entry"] {
            let parent = crate::native::private_tempdir().unwrap();
            let source = acquire(
                &parent.path().join("source"),
                LayoutMode::Source,
                responses(base, dash),
                &format!("http://example.invalid{base}"),
            )
            .unwrap();
            let relative = if dash {
                "video/720p/seg_1.m4s"
            } else {
                "video/720p/seg_0.ts"
            };
            assert_eq!(
                std::fs::read(source.directory().join(relative)).unwrap(),
                b"media bytes"
            );
            assert_eq!(
                source.seal.files[Path::new(relative)],
                bytes_digest(b"media bytes")
            );
            source.check(&Stop::new()).unwrap();
        }
    }
}

#[test]
fn redirected_directory_entrypoints_use_the_effective_directory_as_the_base() {
    for dash in [false, true] {
        for effective in ["/", "/relocated/vod/"] {
            let parent = crate::native::private_tempdir().unwrap();
            let source = acquire_redirected(
                &parent.path().join("source"),
                LayoutMode::Source,
                responses(effective, dash),
                BTreeMap::from([("/old/entry".into(), effective.into())]),
                "http://example.invalid/old/entry",
            )
            .unwrap();
            let relative = if dash {
                "video/720p/seg_1.m4s"
            } else {
                "video/720p/seg_0.ts"
            };
            assert_eq!(
                std::fs::read(source.directory().join(relative)).unwrap(),
                b"media bytes"
            );
            assert!(!source.directory().join("old").exists());
            source.check(&Stop::new()).unwrap();
        }
    }
}
