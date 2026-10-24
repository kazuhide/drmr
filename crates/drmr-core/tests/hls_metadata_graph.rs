//! Master references terminate at media playlists rather than expanding report trees.
mod support;

use drmr_core::{
    conversion::Converter,
    events::NoSink,
    inspection::{self, Inspection, Protection},
    toolrun::{Secrets, ToolRunner},
};
use std::sync::Arc;
use support::{remote_policy, server::TestServer};

const MEDIA: &str = "#EXTM3U\n#EXTINF:2,\nnever-read.ts\n#EXT-X-ENDLIST\n";
const NESTED: &str = "hls-nested-master-playlist";

fn master(reference: &str, count: usize) -> String {
    format!(
        "#EXTM3U\n{}",
        format!("#EXT-X-STREAM-INF:BANDWIDTH=100,CODECS=\"avc1\"\n{reference}\n").repeat(count)
    )
}

fn master_children() -> Vec<String> {
    vec![
        master("never-read.m3u8", 3),
        "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"a\",URI=\"never-read.m3u8\"\n"
            .into(),
        "#EXTM3U\n#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=100,URI=\"never-read.m3u8\"\n".into(),
        "#EXTM3U\n#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"never-read-key\"\n".into(),
        "#EXTM3U\n#EXT-X-SESSION-DATA:DATA-ID=\"example\",VALUE=\"metadata\"\n".into(),
    ]
}

fn command() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            Arc::new(NoSink),
        )),
        "unused",
        "unused",
    )
    .with_decode(true)
}

fn remote(server: &TestServer) -> Result<Inspection, drmr_core::conversion::ConversionError> {
    command().inspect_url_metadata(
        &format!("{}/root.m3u8", server.origin),
        &remote_policy(&server.origin),
    )
}

#[test]
fn local_nested_master_is_refused_instead_of_following_its_references() {
    let directory = drmr_core::native::private_tempdir().unwrap();
    let root = directory.path().join("root.m3u8");
    let child = directory.path().join("child.m3u8");
    std::fs::write(&root, master("child.m3u8", 3)).unwrap();
    // Reading this descendant would produce a different parse error.
    std::fs::write(directory.path().join("never-read.m3u8"), [0xff]).unwrap();
    for nested in master_children() {
        std::fs::write(&child, nested).unwrap();
        assert_eq!(inspection::inspect(&root).unwrap_err().0, NESTED);
    }
}

#[test]
fn remote_nested_master_is_refused_before_any_descendant_or_key_request() {
    for nested in master_children() {
        let server = TestServer::routes(vec![
            (
                "/root.m3u8",
                200,
                vec![],
                master("child.m3u8", 3).into_bytes(),
            ),
            ("/child.m3u8", 200, vec![], nested.into_bytes()),
            ("/never-read.m3u8", 200, vec![], MEDIA.as_bytes().to_vec()),
        ]);
        assert_eq!(remote(&server).unwrap_err().0, NESTED);
        assert_eq!(server.paths(), ["/root.m3u8", "/child.m3u8"]);
    }
}

fn assert_media_tracks(report: &Inspection, expected: usize) {
    assert_eq!(report.protection, Protection::Clear);
    assert!(!report.media_verified);
    assert_eq!(report.tracks.len(), expected);
    assert!(
        report
            .tracks
            .iter()
            .enumerate()
            .all(|(index, track)| track.id == format!("hls-{index:04}"))
    );
    let shown = serde_json::to_string(report).unwrap();
    assert!(!shown.contains("child.m3u8") && !shown.contains("never-read.ts"));
}

#[test]
fn local_root_media_and_128_shared_media_references_remain_bounded_and_valid() {
    let directory = drmr_core::native::private_tempdir().unwrap();
    let root = directory.path().join("root.m3u8");
    let child = directory.path().join("child.m3u8");
    std::fs::write(&child, MEDIA).unwrap();
    for (body, tracks) in [(MEDIA.to_owned(), 1), (master("child.m3u8", 128), 128)] {
        std::fs::write(&root, body).unwrap();
        assert_media_tracks(&inspection::inspect(&root).unwrap(), tracks);
    }
    std::fs::write(&root, master("child.m3u8", 129)).unwrap();
    assert_eq!(
        inspection::inspect(&root).unwrap_err().0,
        "hls-reference-limit"
    );
}

#[test]
fn remote_root_media_and_128_shared_media_references_preserve_cache_and_cap() {
    for (body, tracks, calls) in [
        (MEDIA.to_owned(), 1, 1),
        (master("child.m3u8", 128), 128, 2),
    ] {
        let server = TestServer::routes(vec![
            ("/root.m3u8", 200, vec![], body.into_bytes()),
            ("/child.m3u8", 200, vec![], MEDIA.as_bytes().to_vec()),
        ]);
        assert_media_tracks(&remote(&server).unwrap(), tracks);
        assert_eq!(server.paths().len(), calls);
    }
    let server = TestServer::routes(vec![(
        "/root.m3u8",
        200,
        vec![],
        master("child.m3u8", 129).into_bytes(),
    )]);
    assert_eq!(remote(&server).unwrap_err().0, "hls-reference-limit");
    assert_eq!(server.paths(), ["/root.m3u8"]);
}

#[test]
fn direct_root_cycles_keep_existing_local_findings_and_remote_refusal() {
    let directory = drmr_core::native::private_tempdir().unwrap();
    let root = directory.path().join("root.m3u8");
    std::fs::write(&root, master("root.m3u8", 1)).unwrap();
    let report = inspection::inspect(&root).unwrap();
    assert_eq!(report.protection, Protection::Unknown);
    assert!(
        report
            .findings
            .iter()
            .any(|finding| finding.code == "hls-reference-cycle")
    );
    let server = TestServer::routes(vec![(
        "/root.m3u8",
        200,
        vec![],
        master("root.m3u8", 1).into_bytes(),
    )]);
    assert_eq!(remote(&server).unwrap_err().0, "hls-reference-cycle");
    assert_eq!(server.paths(), ["/root.m3u8"]);
}
