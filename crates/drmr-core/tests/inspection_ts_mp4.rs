//! Metadata-only MP4 candidates require an unambiguous, revalidated HLS selection.
use drmr_core::inspection::{self, OutputFormat, TargetProtection};
use std::fmt::Write as _;

fn source(root: &std::path::Path, video_count: usize, audio_count: usize) -> std::path::PathBuf {
    let mut master = "#EXTM3U\n".to_owned();
    for index in 0..audio_count {
        writeln!(master,
            "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"audio-{index}\",LANGUAGE=\"en\",URI=\"audio-{index}.m3u8\""
        ).unwrap();
        std::fs::write(
            root.join(format!("audio-{index}.m3u8")),
            "#EXTM3U\n#EXTINF:2,\nnot-read.ts\n#EXT-X-ENDLIST\n",
        )
        .unwrap();
    }
    for index in 0..video_count {
        write!(master,
            "#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=320x180,AUDIO=\"a\"\nvideo-{index}.m3u8\n"
        ).unwrap();
        std::fs::write(
            root.join(format!("video-{index}.m3u8")),
            "#EXTM3U\n#EXTINF:2,\nnot-read.ts\n#EXT-X-ENDLIST\n",
        )
        .unwrap();
    }
    let path = root.join("master.m3u8");
    std::fs::write(&path, master).unwrap();
    path
}

#[test]
fn one_video_and_one_audio_offer_only_a_conditional_transport_mp4_candidate() {
    let root = tempfile::tempdir().unwrap();
    let input = source(root.path(), 1, 1);
    let inspected = inspection::inspect(&input).unwrap();
    assert_eq!(inspected.tracks.len(), 2);
    for target in [
        TargetProtection::Clear,
        TargetProtection::Cenc,
        TargetProtection::Cbcs,
    ] {
        let report = inspection::plan(inspected.clone(), target, vec![OutputFormat::Mp4]);
        assert!(
            report.compatible && report.execution_available,
            "{report:?}"
        );
        assert!(!report.input.media_verified);
        assert!(
            !report
                .findings
                .iter()
                .any(|finding| finding.code == "mp4-output-requires-one-track")
        );
        let candidates = report.processing.unwrap().candidates;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].route.as_str(), "fragmented-mp4");
        assert_eq!(candidates[0].output_format, OutputFormat::Mp4);
        assert!(candidates[0].revalidation_required);
        assert!(candidates[0].condition_codes.iter().any(|code| code == "local-transport-stream-single-video-audio-profile-and-presentation-windows-must-pass"));
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 3);
}

#[test]
fn multiple_video_or_audio_renditions_never_advertise_a_standalone_mp4_selection() {
    for (video, audio) in [(6, 1), (1, 2), (2, 0)] {
        let root = tempfile::tempdir().unwrap();
        let input = source(root.path(), video, audio);
        let report = inspection::plan(
            inspection::inspect(&input).unwrap(),
            TargetProtection::Clear,
            vec![OutputFormat::Mp4],
        );
        assert!(!report.execution_available);
        assert_eq!(report.processing.unwrap().candidates, []);
    }
}
