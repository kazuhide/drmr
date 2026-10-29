//! Generated DASH follows movie presentation time, including open video edits.
mod common;

use drmr_core::{
    conversion::{
        ContentKey, ConversionRequest, Converter, HlsInputKeys, InputKeys, OutputKey,
        ProcessingMode, VerificationRequest, VerificationSource, dash_addressing::DashAddressing,
    },
    events::NoSink,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

fn converter(addressing: DashAddressing) -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "decoder-must-not-run",
    )
    .with_ffprobe("probe-must-not-run")
    .with_processing(ProcessingMode::Stitch)
    .with_dash_addressing(addressing)
}

fn source(root: &Path) -> PathBuf {
    fs::create_dir(root).unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let transport = root.join("original.ts");
    let result = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-stream_loop", "-1", "-i"])
        .arg(clip)
        .args([
            "-t",
            "18",
            "-map",
            "0:v:0",
            "-an",
            "-vf",
            "scale=96:54",
            "-c:v",
            "libx264",
            "-threads",
            "1",
            "-preset",
            "veryfast",
            "-bf",
            "2",
            "-g",
            "144",
            "-keyint_min",
            "144",
            "-sc_threshold",
            "0",
            "-output_ts_offset",
            "10",
            "-mpegts_copyts",
            "1",
            "-muxdelay",
            "0",
            "-muxpreload",
            "0",
            "-f",
            "mpegts",
        ])
        .arg(&transport)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "licensed video fixture generation failed"
    );
    let manifest = root.join("master.m3u8");
    let result = Command::new("packager")
        .arg("--single_threaded")
        .arg(format!(
            "in={},stream=video,segment_template={},playlist_name=media.m3u8",
            transport.display(),
            root.join("seg-$Number$.ts").display()
        ))
        .args([
            "--segment_duration",
            "6",
            "--transport_stream_timestamp_offset_ms",
            "0",
            "--hls_playlist_type",
            "VOD",
            "--hls_master_playlist_output",
        ])
        .arg(&manifest)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "licensed HLS fixture generation failed"
    );
    manifest
}

fn source_first_pts(input: &Path) -> u64 {
    let result = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-protocol_whitelist",
            "file,crypto,data",
            "-select_streams",
            "v:0",
            "-show_packets",
            "-show_streams",
            "-show_entries",
            "packet=pts:stream=time_base",
            "-of",
            "json",
        ])
        .arg(input)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "independent fixture clock probe failed"
    );
    let parsed: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(parsed["streams"][0]["time_base"], "1/90000");
    parsed["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|packet| packet["pts"].as_u64().unwrap())
        .min()
        .unwrap()
}

fn assert_manifest(path: &Path, source_pts: u64, addressing: DashAddressing) {
    let bytes = fs::read_to_string(path).unwrap();
    let doc = roxmltree::Document::parse(&bytes).unwrap();
    assert_eq!(
        doc.root_element().attribute("mediaPresentationDuration"),
        Some("PT18.000000000S")
    );
    let node = doc
        .descendants()
        .find(|node| {
            node.is_element()
                && matches!(
                    node.tag_name().name(),
                    "SegmentTemplate" | "SegmentList" | "SegmentBase"
                )
        })
        .unwrap();
    let scale = node.attribute("timescale").unwrap().parse::<u64>().unwrap();
    let offset = node
        .attribute("presentationTimeOffset")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert_eq!(
        u128::from(offset) * 90_000,
        u128::from(source_pts) * u128::from(scale),
        "the manifest must retain the source's movie-presentation origin"
    );
    if addressing == DashAddressing::TemplateDuration {
        let duration = node.attribute("duration").unwrap().parse::<u64>().unwrap();
        assert_eq!(duration, 6 * scale);
        assert!(
            !doc.descendants()
                .any(|node| node.has_tag_name("SegmentTimeline"))
        );
    }
}

#[test]
fn open_video_edits_keep_the_exact_period_and_six_second_grid_in_every_dash_form() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("source"));
    let before = common::snapshot_tree(input.parent().unwrap());
    let source_pts = source_first_pts(&input);
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let key = OutputKey::from_hex(common::KID, common::KEY).unwrap();
    let decrypt = InputKeys::new(vec![
        ContentKey::from_hex(common::KID, common::KEY).unwrap(),
    ])
    .unwrap();
    for target in [
        TargetProtection::Clear,
        TargetProtection::Cenc,
        TargetProtection::Cbcs,
    ] {
        for (name, addressing) in [
            ("nominal", DashAddressing::TemplateDuration),
            ("timeline", DashAddressing::Template),
            ("list", DashAddressing::List),
            ("base", DashAddressing::Base),
        ] {
            let output = root.path().join(format!("{}-{name}", target.as_str()));
            let command = converter(addressing);
            let (result, measurement) = command.measure(|command| {
                command.convert(ConversionRequest {
                    input: &input,
                    output: &output,
                    format: OutputFormat::Dash,
                    target,
                    input_keys: &empty,
                    hls_input_keys: &hls,
                    output_key: (target != TargetProtection::Clear).then_some(&key),
                    hls_key_uri: None,
                    hls_output_key: None,
                })
            });
            assert!(result.unwrap().verification.passed);
            let manifest = output.join("manifest.mpd");
            assert_manifest(&manifest, source_pts, addressing);
            let report = command
                .verify(VerificationRequest {
                    source: VerificationSource {
                        input: &input,
                        input_keys: &empty,
                        hls_keys: &hls,
                    },
                    output: VerificationSource {
                        input: &manifest,
                        input_keys: if target == TargetProtection::Clear {
                            &empty
                        } else {
                            &decrypt
                        },
                        hls_keys: &hls,
                    },
                })
                .unwrap();
            assert!(report.passed);
            let measured = serde_json::to_value(measurement).unwrap();
            for tool in measured["tools"].as_array().unwrap() {
                if tool["tool"] != "packager" {
                    assert_eq!(tool["calls"], 0);
                }
            }
        }
    }
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), before);
}
