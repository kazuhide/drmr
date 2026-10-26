//! Repackaging inherits the admitted HLS target without changing sample evidence.
#[path = "support/segment_target_paths.rs"]
mod additional;
mod common;
#[path = "support/segment_target_lexical.rs"]
mod lexical;

use drmr_core::{
    conversion::{
        Converter, HlsInputKeys, InputKeys, ProcessingMode, ValidationDepth, VerificationSource,
        conformance::ManifestValidationTools,
        key_file::BundleOperatorKeys,
        plan::bundle::{BundleOutputIntent, BundlePlanRequest, ManifestValidation},
    },
    events::NoSink,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{fs, path::Path, sync::Arc};

fn source(directory: &Path, rate: u32) -> std::path::PathBuf {
    fs::create_dir(directory).unwrap();
    let input = directory.join("media.m3u8");
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let result = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-threads",
            "1",
            "-stream_loop",
            "1",
            "-i",
        ])
        .arg(clip)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-vf",
            "setpts=N/(24*TB)",
            "-r",
            "24",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-threads:v",
            "1",
            "-g",
            "144",
            "-keyint_min",
            "144",
            "-sc_threshold",
            "0",
            "-bf",
            "0",
            "-af",
            "asetpts=N/SR/TB",
            "-c:a",
            "aac",
            "-ar",
        ])
        .arg(rate.to_string())
        .args([
            "-hls_time",
            "6",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(directory.join("segment-%d.ts"))
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "six-second BBB fixture generation failed"
    );
    let text = fs::read_to_string(&input).unwrap();
    assert!(text.lines().any(|line| line == "#EXT-X-TARGETDURATION:6"));
    let durations = hls_durations(&text);
    assert_eq!(durations.len(), 4);
    assert!(durations.last().unwrap() < &6.0);
    input
}

fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "a-decoder-that-must-not-run",
    )
    .with_ffprobe("a-probe-that-must-not-run")
    .with_processing(ProcessingMode::Stitch)
}

fn hls_durations(text: &str) -> Vec<f64> {
    text.lines()
        .filter_map(|line| line.strip_prefix("#EXTINF:"))
        .map(|value| value.split(',').next().unwrap().parse().unwrap())
        .collect()
}

fn assert_six_second_segments(values: &[f64]) {
    // Shaka uses an absolute clock grid. A nonzero source origin can produce
    // partial first/last segments; the complete interior cadence must stay six.
    assert!(
        (4..=5).contains(&values.len()),
        "a twenty-second track must use its inherited six-second target: {values:?}"
    );
    assert!(
        values
            .iter()
            .all(|duration| *duration > 0.0 && *duration < 6.1)
    );
    assert!(
        values[1..values.len() - 1]
            .iter()
            .all(|duration| (5.9..6.1).contains(duration)),
        "ordinary segments must stay near six seconds: {values:?}"
    );
    assert!(
        values[0] < 5.9 || *values.last().unwrap() < 5.9,
        "the finite source must retain a partial edge: {values:?}"
    );
}

fn assert_outputs(root: &Path, hls_ids: &[&str]) {
    for id in hls_ids {
        let directory = root.join(id);
        let master = fs::read_to_string(directory.join("master.m3u8")).unwrap();
        let mut media = Vec::new();
        for line in master.lines() {
            if line.starts_with("#EXT-X-MEDIA:") {
                let uri = line
                    .split("URI=\"")
                    .nth(1)
                    .unwrap()
                    .split('"')
                    .next()
                    .unwrap();
                media.push(uri.to_string());
            } else if !line.is_empty() && !line.starts_with('#') {
                media.push(line.to_string());
            }
        }
        media.sort();
        media.dedup();
        assert_eq!(media.len(), 2);
        for uri in media {
            let text = fs::read_to_string(directory.join(uri)).unwrap();
            assert!(
                text.lines().any(|line| line == "#EXT-X-TARGETDURATION:6"),
                "the inherited target declaration must stay six seconds: {text}"
            );
            assert_six_second_segments(&hls_durations(&text));
        }
    }
    let text = fs::read_to_string(root.join("dash/manifest.mpd")).unwrap();
    let document = roxmltree::Document::parse(&text).unwrap();
    let representations: Vec<_> = document
        .descendants()
        .filter(|node| node.has_tag_name("Representation"))
        .collect();
    assert_eq!(representations.len(), 2);
    for representation in representations {
        let template = representation
            .children()
            .find(|node| node.has_tag_name("SegmentTemplate"))
            .unwrap();
        let scale: f64 = template.attribute("timescale").unwrap().parse().unwrap();
        let mut durations = Vec::new();
        for item in template.descendants().filter(|node| node.has_tag_name("S")) {
            let duration: f64 = item.attribute("d").unwrap().parse().unwrap();
            let repeat: usize = item.attribute("r").unwrap_or("0").parse().unwrap();
            assert!(repeat < 16);
            durations.extend(std::iter::repeat_n(duration / scale, repeat + 1));
        }
        assert_six_second_segments(&durations);
    }
}

#[test]
fn six_second_hls_target_reaches_every_saved_bundle_output() {
    for rate in [44_100, 48_000] {
        let root = common::private_tempdir().unwrap();
        let input = source(&root.path().join("source"), rate);
        let original = common::snapshot_tree(input.parent().unwrap());
        let outputs: Vec<_> = [
            ("hls", OutputFormat::Hls),
            ("dash", OutputFormat::Dash),
            ("ts", OutputFormat::HlsTs),
        ]
        .into_iter()
        .map(|(id, format)| BundleOutputIntent {
            dash_addressing: None,
            id: id.into(),
            format,
            target: TargetProtection::Clear,
            drms: Vec::new(),
            conformance: ManifestValidation::None,
            processing: None,
        })
        .collect();
        let converter = converter();
        let destination = root.path().join("result");
        let plan = converter
            .create_bundle_plan(BundlePlanRequest {
                input: VerificationSource {
                    input: &input,
                    input_keys: &InputKeys::default(),
                    hls_keys: &HlsInputKeys::default(),
                },
                output_root: &destination,
                outputs: &outputs,
            })
            .unwrap();
        let keys = BundleOperatorKeys::default();
        let tools = ManifestValidationTools::default();
        let report = converter.execute_bundle_plan(&plan, &keys, &tools).unwrap();
        for output in report.outputs {
            assert!(output.conversion.verification.passed);
            assert_eq!(
                output.conversion.verification.required_depth,
                ValidationDepth::Native
            );
        }
        assert_outputs(&destination, &["hls", "ts"]);
        assert!(
            plan.processing
                .as_ref()
                .unwrap()
                .iter()
                .all(|choice| choice.selection.segment_duration_ms == Some(6000))
        );
        let verified = converter.verify_bundle_plan(&plan, &keys, &tools).unwrap();
        assert!(verified.passed && verified.source_unchanged);
        assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
    }
}
