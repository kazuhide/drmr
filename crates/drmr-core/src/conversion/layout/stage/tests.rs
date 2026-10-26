//! Byte and identity regressions are separate from media-semantic qualification.
use super::*;
use crate::{
    conversion::{files, layout::SourceTrack},
    inspection::TrackKind,
    native::FileIdentity,
};
use std::{fs, io::Write as _};

fn write(stage: &Stage, name: &str, bytes: &[u8]) {
    let path = stage.path().join(name);
    files::create_dir(path.parent().unwrap()).unwrap();
    files::create_new(&path).unwrap().write_all(bytes).unwrap();
}

fn fixture() -> (Stage, SourceLayout) {
    let stage = Stage::new(None).unwrap();
    write(&stage, "master.m3u8", b"#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"stereo\",NAME=\"Audio\",URI=\"track-0000/media.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"stereo\"\ntrack-0001/media.m3u8\n");
    for ordinal in 0..2 {
        write(
            &stage,
            &format!("track-{ordinal:04}/init.mp4"),
            b"unchanged initialization",
        );
        write(
            &stage,
            &format!("track-{ordinal:04}/seg_1.m4s"),
            format!("unchanged segment {ordinal}").as_bytes(),
        );
        write(&stage, &format!("track-{ordinal:04}/media.m3u8"), b"#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://operator-value\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6,\nseg_1.m4s\n#EXT-X-ENDLIST\n");
    }
    let tracks = [
        SourceTrack {
            source: "audio.m3u8".into(),
            selector: 0,
            kind: TrackKind::Audio,
            playlist: Some("audio.m3u8".into()),
            media: vec!["shared/audio/seg_0.ts".into()],
        },
        SourceTrack {
            source: "video.m3u8".into(),
            selector: 0,
            kind: TrackKind::Video,
            playlist: Some("video.m3u8".into()),
            media: vec!["shared/video/seg_0.ts".into()],
        },
    ];
    let layout =
        SourceLayout::resolve_from_tracks(LayoutMode::Source, OutputFormat::Hls, &tracks).unwrap();
    (stage, layout)
}

#[test]
fn re_layout_moves_media_without_copying_and_rewrites_actual_hls_paths() {
    let (stage, layout) = fixture();
    let old = FileIdentity::read(
        &files::regular_input(&stage.path().join("track-0000/seg_1.m4s")).unwrap(),
    )
    .unwrap();
    let output = layout
        .apply_to_stage(&stage, OutputFormat::Hls, &Stop::new())
        .unwrap();
    assert_eq!(
        output.tracks[0].media,
        [PathBuf::from("shared/audio/track-0000-seg_1.m4s")]
    );
    let path = stage.path().join(&output.tracks[0].media[0]);
    assert_eq!(fs::read(&path).unwrap(), b"unchanged segment 0");
    assert!(FileIdentity::read(&files::regular_input(&path).unwrap()).unwrap() == old);
    let master = fs::read_to_string(stage.path().join("master.m3u8")).unwrap();
    assert!(master.contains("URI=\"audio.m3u8\""));
    assert!(master.contains("\nvideo.m3u8\n"));
    let audio = fs::read_to_string(stage.path().join("audio.m3u8")).unwrap();
    assert!(audio.contains("URI=\"shared/audio/track-0000-init.mp4\""));
    assert!(audio.contains("\nshared/audio/track-0000-seg_1.m4s\n"));
    assert!(audio.contains("skd://operator-value"));
    layout
        .verify_actual_output(stage.path(), OutputFormat::Hls, &Stop::new())
        .unwrap();
    assert!(!stage.path().join("track-0000").exists());
    assert!(!stage.path().join("track-0001").exists());
}

#[test]
fn final_layout_check_detects_cross_track_media_references() {
    let (stage, layout) = fixture();
    layout
        .apply_to_stage(&stage, OutputFormat::Hls, &Stop::new())
        .unwrap();
    let path = stage.path().join("audio.m3u8");
    let text = fs::read_to_string(&path).unwrap().replace(
        "shared/audio/track-0000-seg_1.m4s",
        "shared/video/track-0001-seg_1.m4s",
    );
    fs::write(path, text).unwrap();
    assert!(
        layout
            .verify_actual_output(stage.path(), OutputFormat::Hls, &Stop::new())
            .is_err()
    );
}

#[test]
fn shared_parent_keeps_tracks_separate_with_explicit_media_lists() {
    let (stage, mut layout) = fixture();
    for placement in &mut layout.tracks {
        placement.media_directory = "shared".into();
    }
    let output = layout
        .apply_to_stage(&stage, OutputFormat::Hls, &Stop::new())
        .unwrap();
    assert_eq!(
        output.tracks[0].media,
        [PathBuf::from("shared/track-0000-seg_1.m4s")]
    );
    assert_eq!(
        output.tracks[1].media,
        [PathBuf::from("shared/track-0001-seg_1.m4s")]
    );
    assert_eq!(
        fs::read(stage.path().join(&output.tracks[1].media[0])).unwrap(),
        b"unchanged segment 1"
    );
}

#[test]
fn source_paths_may_overlap_the_original_generated_directories() {
    let (stage, mut layout) = fixture();
    layout.tracks[0].playlist = "track-0001/media.m3u8".into();
    layout.tracks[0].media_directory = "track-0001/audio".into();
    layout.tracks[1].playlist = "track-0000/media.m3u8".into();
    layout.tracks[1].media_directory = "track-0000/video".into();
    let output = layout
        .apply_to_stage(&stage, OutputFormat::Hls, &Stop::new())
        .unwrap();
    assert_eq!(
        fs::read(stage.path().join(&output.tracks[0].media[0])).unwrap(),
        b"unchanged segment 0"
    );
    assert_eq!(
        fs::read(stage.path().join(&output.tracks[1].media[0])).unwrap(),
        b"unchanged segment 1"
    );
}

#[test]
fn cancellation_precedes_any_output_move() {
    let (stage, layout) = fixture();
    let stop = Stop::new();
    stop.set();
    assert!(matches!(
        layout.apply_to_stage(&stage, OutputFormat::Hls, &stop),
        Err(ConversionError("cancelled"))
    ));
    assert!(stage.path().join("track-0000/seg_1.m4s").is_file());
    assert!(!stage.path().join("shared").exists());
}

#[test]
fn segment_transport_keeps_a_multiplexed_rendition_in_one_physical_location() {
    let stage = Stage::new(None).unwrap();
    write(&stage, "master.m3u8", b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000,CODECS=\"avc1.64001f,mp4a.40.2\"\ntrack-0000/media.m3u8\n");
    write(
        &stage,
        "track-0000/media.m3u8",
        b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nsegment-00000.ts\n#EXT-X-ENDLIST\n",
    );
    write(
        &stage,
        "track-0000/segment-00000.ts",
        b"unchanged multiplexed resource",
    );
    let tracks = [
        SourceTrack {
            source: "muxed.m3u8".into(),
            selector: 0,
            kind: TrackKind::Video,
            playlist: Some("muxed.m3u8".into()),
            media: vec!["media/segment.ts".into()],
        },
        SourceTrack {
            source: "muxed.m3u8".into(),
            selector: 1,
            kind: TrackKind::Audio,
            playlist: Some("muxed.m3u8".into()),
            media: vec!["media/segment.ts".into()],
        },
    ];
    let layout = SourceLayout::resolve_for_processing(
        LayoutMode::Source,
        OutputFormat::HlsTs,
        crate::conversion::ProcessingMode::Segments,
        &tracks,
    )
    .unwrap();
    let output = layout
        .apply_to_stage(&stage, OutputFormat::HlsTs, &Stop::new())
        .unwrap();
    assert_eq!(output.tracks.len(), 2);
    assert_eq!(output.tracks[0].media, output.tracks[1].media);
    assert_eq!(output.tracks[0].playlist, Path::new("muxed.m3u8"));
    assert_eq!(output.files.len(), 2);
    assert_eq!(
        fs::read(stage.path().join(&output.tracks[0].media[0])).unwrap(),
        b"unchanged multiplexed resource"
    );
    layout
        .verify_actual_output(stage.path(), OutputFormat::HlsTs, &Stop::new())
        .unwrap();
}

#[test]
fn dash_references_are_rewritten_before_the_relocated_stage_is_returned() {
    let stage = Stage::new(None).unwrap();
    write(&stage, "manifest.mpd", br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT12S"><Period duration="PT12S"><AdaptationSet contentType="video"><Representation id="v" bandwidth="1000" mimeType="video/mp4" codecs="avc1.64001f"><SegmentTemplate timescale="1" initialization="track-0000/init.mp4" media="track-0000/seg_$Number$.m4s" startNumber="1"><SegmentTimeline><S t="0" d="6" r="1"/></SegmentTimeline></SegmentTemplate></Representation></AdaptationSet></Period></MPD>"#);
    write(&stage, "track-0000/init.mp4", b"unchanged initialization");
    write(&stage, "track-0000/seg_1.m4s", b"first unchanged media");
    write(&stage, "track-0000/seg_2.m4s", b"second unchanged media");
    let track = SourceTrack {
        source: "video.m3u8".into(),
        selector: 0,
        kind: TrackKind::Video,
        playlist: Some("video.m3u8".into()),
        media: vec!["video/1080p high/source.ts".into()],
    };
    let layout =
        SourceLayout::resolve_from_tracks(LayoutMode::Source, OutputFormat::Dash, &[track])
            .unwrap();
    layout
        .apply_to_stage(&stage, OutputFormat::Dash, &Stop::new())
        .unwrap();
    let manifest = fs::read_to_string(stage.path().join("manifest.mpd")).unwrap();
    assert!(manifest.contains("initialization=\"video/1080p%20high/track-0000-init.mp4\""));
    assert!(manifest.contains("media=\"video/1080p%20high/track-0000-seg_$Number$.m4s\""));
    assert!(!manifest.contains("track-0000/"));
    layout
        .verify_actual_output(stage.path(), OutputFormat::Dash, &Stop::new())
        .unwrap();
}

#[test]
fn captured_media_replacement_is_never_adopted_for_relocation() {
    let stage = Stage::new(None).unwrap();
    write(&stage, "track-0000/seg_1.m4s", b"original segment");
    let mut tree = StageRelocation::new(&stage).unwrap();
    tree.capture_directory(Path::new("track-0000")).unwrap();
    tree.capture_file(Path::new("track-0000/seg_1.m4s"))
        .unwrap();
    tree.prepare_parents(Path::new("video/seg_1.m4s")).unwrap();
    fs::rename(
        stage.path().join("track-0000/seg_1.m4s"),
        stage.path().join("held-original.m4s"),
    )
    .unwrap();
    write(&stage, "track-0000/seg_1.m4s", b"replacement segment");
    assert!(
        tree.move_file(
            Path::new("track-0000/seg_1.m4s"),
            Path::new("video/seg_1.m4s")
        )
        .is_err()
    );
    assert_eq!(
        fs::read(stage.path().join("track-0000/seg_1.m4s")).unwrap(),
        b"replacement segment"
    );
    assert!(!stage.path().join("video/seg_1.m4s").exists());
}

#[test]
fn relocation_never_overwrites_an_unregistered_destination_file() {
    let stage = Stage::new(None).unwrap();
    write(&stage, "track-0000/seg_1.m4s", b"original segment");
    let mut tree = StageRelocation::new(&stage).unwrap();
    tree.capture_directory(Path::new("track-0000")).unwrap();
    tree.capture_file(Path::new("track-0000/seg_1.m4s"))
        .unwrap();
    tree.prepare_parents(Path::new("video/seg_1.m4s")).unwrap();
    write(&stage, "video/seg_1.m4s", b"unregistered destination");
    assert!(
        tree.move_file(
            Path::new("track-0000/seg_1.m4s"),
            Path::new("video/seg_1.m4s")
        )
        .is_err()
    );
    assert_eq!(
        fs::read(stage.path().join("video/seg_1.m4s")).unwrap(),
        b"unregistered destination"
    );
    assert!(stage.path().join("track-0000/seg_1.m4s").is_file());
}

#[cfg(unix)]
#[test]
fn an_unregistered_symlink_target_is_rejected_before_moving_media() {
    let (stage, layout) = fixture();
    let external = crate::native::private_tempdir().unwrap();
    fs::write(external.path().join("sentinel"), b"untouched").unwrap();
    std::os::unix::fs::symlink(external.path(), stage.path().join("shared")).unwrap();
    assert!(
        layout
            .apply_to_stage(&stage, OutputFormat::Hls, &Stop::new())
            .is_err()
    );
    assert_eq!(
        fs::read(external.path().join("sentinel")).unwrap(),
        b"untouched"
    );
    assert!(stage.path().join("track-0000/seg_1.m4s").is_file());
}

#[cfg(unix)]
#[test]
fn replacing_a_prepared_parent_never_redirects_a_file_move() {
    let stage = Stage::new(None).unwrap();
    write(&stage, "track-0000/seg_1.m4s", b"original segment");
    let mut tree = StageRelocation::new(&stage).unwrap();
    tree.capture_directory(Path::new("track-0000")).unwrap();
    tree.capture_file(Path::new("track-0000/seg_1.m4s"))
        .unwrap();
    tree.prepare_parents(Path::new("video/720p/seg_1.m4s"))
        .unwrap();
    let external = crate::native::private_tempdir().unwrap();
    fs::rename(stage.path().join("video"), stage.path().join("displaced")).unwrap();
    std::os::unix::fs::symlink(external.path(), stage.path().join("video")).unwrap();
    assert!(
        tree.move_file(
            Path::new("track-0000/seg_1.m4s"),
            Path::new("video/720p/seg_1.m4s")
        )
        .is_err()
    );
    assert!(fs::read_dir(external.path()).unwrap().next().is_none());
    assert!(stage.path().join("track-0000/seg_1.m4s").is_file());
}
