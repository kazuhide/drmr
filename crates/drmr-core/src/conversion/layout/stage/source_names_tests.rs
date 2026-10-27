//! Source filenames and manifest references must describe the same final objects.
use super::*;
use crate::{conversion::layout::SourceTrack, inspection::TrackKind};
use std::{fmt::Write as _, fs, io::Write};

fn write(stage: &Stage, path: &str, bytes: &[u8]) {
    let path = stage.path().join(path);
    crate::conversion::files::create_dir(path.parent().unwrap()).unwrap();
    crate::conversion::files::create_new(&path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn fixture(format: OutputFormat, originals: &[&str]) -> (Stage, SourceLayout) {
    let stage = Stage::new(None).unwrap();
    let extension = if format == OutputFormat::HlsTs {
        "ts"
    } else {
        "m4s"
    };
    if format == OutputFormat::Dash {
        write(&stage, "manifest.mpd", br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT12S"><Period duration="PT12S"><AdaptationSet contentType="video"><Representation id="v" bandwidth="1000" mimeType="video/mp4" codecs="avc1.64001f"><SegmentTemplate timescale="1" initialization="track-0000/init.mp4" media="track-0000/seg_$Number$.m4s" startNumber="1"><SegmentTimeline><S t="0" d="6" r="1"/></SegmentTimeline></SegmentTemplate></Representation></AdaptationSet></Period></MPD>"#);
    } else {
        write(
            &stage,
            "master.m3u8",
            b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\ntrack-0000/media.m3u8\n",
        );
        let mut media = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n".to_owned();
        if format != OutputFormat::HlsTs {
            media.push_str("#EXT-X-MAP:URI=\"init.mp4\"\n");
        }
        for index in 1..=2 {
            writeln!(media, "#EXTINF:6,\n{}", internal_name(index, extension)).unwrap();
        }
        media.push_str("#EXT-X-ENDLIST\n");
        write(&stage, "track-0000/media.m3u8", media.as_bytes());
    }
    if format != OutputFormat::HlsTs {
        write(
            &stage,
            "track-0000/init.mp4",
            b"original initialization bytes",
        );
    }
    for index in 1..=2 {
        write(
            &stage,
            &format!("track-0000/{}", internal_name(index, extension)),
            format!("original media {index}").as_bytes(),
        );
    }
    let track = SourceTrack {
        source: "video.m3u8".into(),
        selector: 0,
        kind: TrackKind::Video,
        playlist: Some("video.m3u8".into()),
        media: originals
            .iter()
            .map(|name| PathBuf::from("video/720p").join(name))
            .collect(),
    };
    let layout = SourceLayout::resolve_from_tracks(LayoutMode::Source, format, &[track]).unwrap();
    (stage, layout)
}

fn internal_name(index: usize, extension: &str) -> String {
    if extension == "ts" {
        format!("segment-{index}.ts")
    } else {
        format!("seg_{index}.m4s")
    }
}

#[test]
fn hls_retains_original_sequence_start_padding_and_media_bytes() {
    for format in [OutputFormat::Hls, OutputFormat::HlsTs] {
        let (stage, layout) = fixture(format, &["r000600.ts", "r000601.ts"]);
        let output = layout
            .apply_to_stage(&stage, format, DashAddressing::Template, &Stop::new())
            .unwrap();
        let extension = if format == OutputFormat::HlsTs {
            "ts"
        } else {
            "m4s"
        };
        assert_eq!(
            output.tracks[0].media,
            [
                PathBuf::from(format!("video/720p/r000600.{extension}")),
                PathBuf::from(format!("video/720p/r000601.{extension}"))
            ]
        );
        assert_eq!(
            fs::read(stage.path().join(&output.tracks[0].media[0])).unwrap(),
            b"original media 1"
        );
        if format == OutputFormat::Hls {
            assert_eq!(
                output.tracks[0].initialization,
                Some(PathBuf::from("video/720p/init.mp4"))
            );
        }
        layout
            .verify_actual_output(stage.path(), format, DashAddressing::Template, &Stop::new())
            .unwrap();
        assert!(
            !fs::read_to_string(stage.path().join("video.m3u8"))
                .unwrap()
                .contains("track-0000")
        );
    }
}

#[test]
fn dash_rebases_numbering_without_changing_the_media_clock() {
    for addressing in [DashAddressing::Template, DashAddressing::List] {
        let (stage, layout) = fixture(OutputFormat::Dash, &["r000600.ts", "r000601.ts"]);
        layout
            .apply_to_stage(&stage, OutputFormat::Dash, addressing, &Stop::new())
            .unwrap();
        crate::conversion::dash::rewrite_output(stage.path(), addressing, Some(6000), &Stop::new())
            .unwrap();
        let manifest = fs::read_to_string(stage.path().join("manifest.mpd")).unwrap();
        assert!(manifest.contains("startNumber=\"600\""), "{manifest}");
        assert!(manifest.contains("t=\"0\" d=\"6\""));
        assert!(manifest.contains("video/720p/init.mp4"));
        if addressing == DashAddressing::Template {
            assert!(manifest.contains("r$Number%06d$.m4s"));
        } else {
            assert!(manifest.contains("video/720p/r000600.m4s"));
        }
        layout
            .verify_actual_output(stage.path(), OutputFormat::Dash, addressing, &Stop::new())
            .unwrap();
    }
}

#[test]
fn arbitrary_names_are_retained_when_the_manifest_can_list_them() {
    for (format, addressing) in [
        (OutputFormat::Hls, DashAddressing::Template),
        (OutputFormat::Dash, DashAddressing::List),
    ] {
        let (stage, layout) = fixture(format, &["opening.ts", "closing.ts"]);
        let output = layout
            .apply_to_stage(&stage, format, addressing, &Stop::new())
            .unwrap();
        assert_eq!(
            output.tracks[0].media,
            [
                PathBuf::from("video/720p/opening.m4s"),
                PathBuf::from("video/720p/closing.m4s")
            ]
        );
        if format == OutputFormat::Dash {
            crate::conversion::dash::rewrite_output(
                stage.path(),
                addressing,
                Some(6000),
                &Stop::new(),
            )
            .unwrap();
        }
        layout
            .verify_actual_output(stage.path(), format, addressing, &Stop::new())
            .unwrap();
    }
}

#[test]
fn a_number_template_uses_unprefixed_names_if_source_names_are_not_representable() {
    let (stage, layout) = fixture(OutputFormat::Dash, &["opening.ts", "closing.ts"]);
    let output = layout
        .apply_to_stage(
            &stage,
            OutputFormat::Dash,
            DashAddressing::Template,
            &Stop::new(),
        )
        .unwrap();
    assert_eq!(
        output.tracks[0].media,
        [
            PathBuf::from("video/720p/seg_1.m4s"),
            PathBuf::from("video/720p/seg_2.m4s")
        ]
    );
    layout
        .verify_actual_output(
            stage.path(),
            OutputFormat::Dash,
            DashAddressing::Template,
            &Stop::new(),
        )
        .unwrap();
}
