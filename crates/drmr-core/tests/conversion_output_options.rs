//! Final manifest references and resource directories are verified after output selection.
mod common;

use drmr_core::{
    conversion::{
        ConversionRequest, Converter, HlsInputKeys, InputKeys, ProcessingMode,
        dash_addressing::DashAddressing,
    },
    events::NoSink,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "decoder-must-not-run",
    )
    .with_ffprobe("probe-must-not-run")
}

fn clear(converter: &Converter, input: &Path, output: &Path, format: OutputFormat) {
    let result = converter
        .convert(ConversionRequest {
            input,
            output,
            format,
            target: TargetProtection::Clear,
            input_keys: &InputKeys::default(),
            hls_input_keys: &HlsInputKeys::default(),
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap();
    assert!(result.verification.passed);
}

fn source(root: &Path) -> PathBuf {
    let source = root.join("source");
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    clear(&converter(), &clip, &source, OutputFormat::Hls);
    fs::create_dir_all(source.join("video")).unwrap();
    fs::create_dir_all(source.join("audio")).unwrap();
    fs::rename(source.join("track-0000"), source.join("video/720p")).unwrap();
    fs::rename(source.join("track-0001"), source.join("audio/en")).unwrap();
    let master = source.join("master.m3u8");
    let text = fs::read_to_string(&master)
        .unwrap()
        .replace("track-0000/", "video/720p/")
        .replace("track-0001/", "audio/en/");
    fs::write(&master, text).unwrap();
    master
}

#[test]
fn new_operations_keep_media_directories_and_use_the_selected_dash_form() {
    let directory = common::private_tempdir().unwrap();
    let input = source(directory.path());
    let before = common::snapshot_tree(input.parent().unwrap());
    for (name, addressing, element, timeline) in [
        (
            "template",
            DashAddressing::TemplateDuration,
            "SegmentTemplate",
            false,
        ),
        (
            "timeline",
            DashAddressing::Template,
            "SegmentTemplate",
            true,
        ),
        ("list", DashAddressing::List, "SegmentList", true),
        ("base", DashAddressing::Base, "SegmentBase", false),
    ] {
        for processing in [ProcessingMode::Stitch, ProcessingMode::Segments] {
            let output = directory.path().join(format!("{name}-{processing:?}"));
            clear(
                &converter()
                    .with_new_output_defaults()
                    .with_dash_addressing(addressing)
                    .with_processing(processing),
                &input,
                &output,
                OutputFormat::Dash,
            );
            assert!(
                output.join("video/720p").is_dir(),
                "source video directory is missing"
            );
            assert!(
                output.join("audio/en").is_dir(),
                "source audio directory is missing"
            );
            assert!(!output.join("track-0000").exists());
            let text = fs::read_to_string(output.join("manifest.mpd")).unwrap();
            let xml = roxmltree::Document::parse(&text).unwrap();
            assert_eq!(
                xml.descendants()
                    .filter(|n| n.has_tag_name(element))
                    .count(),
                2
            );
            assert_eq!(
                xml.descendants().any(|n| n.has_tag_name("SegmentTimeline")),
                timeline
            );
        }
    }
    for processing in [ProcessingMode::Stitch, ProcessingMode::Segments] {
        let output = directory.path().join(format!("hls-{processing:?}"));
        clear(
            &converter()
                .with_new_output_defaults()
                .with_processing(processing),
            &input,
            &output,
            OutputFormat::Hls,
        );
        let text = fs::read_to_string(output.join("master.m3u8")).unwrap();
        assert!(text.contains("video/720p/media.m3u8"));
        assert!(text.contains("audio/en/media.m3u8"));
        assert!(!text.contains("track-0000/"));
    }
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), before);
}

#[test]
fn transport_renditions_keep_audio_and_video_in_their_original_directories() {
    let directory = common::private_tempdir().unwrap();
    let root = directory.path().join("source");
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    for (path, selector) in [("audio/en", "0:a:0"), ("video/720p", "0:v:0")] {
        let child = root.join(path);
        fs::create_dir_all(&child).unwrap();
        let status = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(&clip)
            .args([
                "-map",
                selector,
                "-c",
                "copy",
                "-hls_time",
                "2",
                "-hls_playlist_type",
                "vod",
                "-hls_segment_filename",
            ])
            .arg(child.join("segment-%d.ts"))
            .arg(child.join("media.m3u8"))
            .status()
            .unwrap();
        assert!(status.success());
    }
    let input = root.join("master.m3u8");
    fs::write(&input, "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,URI=\"audio/en/media.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,AUDIO=\"audio\"\nvideo/720p/media.m3u8\n").unwrap();
    let before = common::snapshot_tree(&root);
    for processing in [ProcessingMode::Stitch, ProcessingMode::Segments] {
        let output = directory.path().join(format!("result-{processing:?}"));
        clear(
            &converter()
                .with_new_output_defaults()
                .with_processing(processing),
            &input,
            &output,
            OutputFormat::HlsTs,
        );
        let master = fs::read_to_string(output.join("master.m3u8")).unwrap();
        let audio = master
            .lines()
            .find(|line| line.starts_with("#EXT-X-MEDIA:"))
            .unwrap();
        assert!(
            audio.contains("URI=\"audio/en/media.m3u8\""),
            "audio changed directories: {master}"
        );
        assert!(master.lines().any(|line| line == "video/720p/media.m3u8"));
    }
    assert_eq!(common::snapshot_tree(&root), before);
}
