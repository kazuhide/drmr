//! Flat AES output uses the admitted source target before independent verification.
use super::*;
use crate::{
    conversion::{ProcessingMode, VerificationRequest, VerificationSource},
    events::Collector,
    toolrun::{Secrets, ToolRunner},
};
use std::sync::Arc;

fn source(root: &Path) -> PathBuf {
    std::fs::create_dir(root).unwrap();
    let path = root.join("media.m3u8");
    let result = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-nostdin",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=997:sample_rate=44100:duration=20",
            "-c:a",
            "aac",
            "-threads",
            "1",
            "-hls_segment_type",
            "fmp4",
            "-hls_time",
            "6",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(root.join("segment-%d.m4s"))
        .arg(&path)
        .output()
        .unwrap();
    assert!(result.status.success(), "AAC fixture generation failed");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("#EXT-X-TARGETDURATION:6\n"));
    assert!(
        text.lines()
            .filter_map(|line| line.strip_prefix("#EXTINF:"))
            .any(|value| {
                let duration = value.split(',').next().unwrap().parse::<f64>().unwrap();
                duration > 6.0 && duration < 6.5
            })
    );
    path
}

#[test]
fn public_aes_helper_preserves_six_second_aac_headers_for_auto_stitch_and_segments() {
    let root = crate::native::private_tempdir().unwrap();
    let input = source(&root.path().join("source"));
    let original = std::fs::read(&input).unwrap();
    let output_key = HlsOutputKey::new(
        "https://synthetic.invalid/output",
        Aes128Key::from_hex(&"31".repeat(16)).unwrap(),
    )
    .unwrap();
    let output_keys = HlsInputKeys::new(vec![(
        output_key.key_uri().into(),
        Aes128Key::from_hex(&"31".repeat(16)).unwrap(),
    )])
    .unwrap();
    for (ordinal, mode) in [
        ProcessingMode::Auto,
        ProcessingMode::Stitch,
        ProcessingMode::Segments,
    ]
    .into_iter()
    .enumerate()
    {
        let events = Arc::new(Collector::default());
        let command = Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
            "packager",
            "must-not-run-decoder",
        )
        .with_ffprobe("must-not-run-probe")
        .with_processing(mode);
        let output = root.path().join(format!("result-{ordinal}"));
        let report = command
            .convert_hls_to_hls_aes128(
                &input,
                &output,
                &InputKeys::default(),
                &HlsInputKeys::default(),
                &output_key,
            )
            .unwrap();
        assert!(report.verification.passed);
        let playlist = output.join(if mode == ProcessingMode::Segments {
            "track-0000/media.m3u8"
        } else {
            "media.m3u8"
        });
        let text = std::fs::read_to_string(&playlist).unwrap();
        assert!(
            text.contains("#EXT-X-TARGETDURATION:6\n"),
            "{mode:?}: inherited AAC target was changed"
        );
        let verified = command
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &input,
                    input_keys: &InputKeys::default(),
                    hls_keys: &HlsInputKeys::default(),
                },
                output: VerificationSource {
                    input: &playlist,
                    input_keys: &InputKeys::default(),
                    hls_keys: &output_keys,
                },
            })
            .unwrap();
        assert!(verified.passed);
        for record in events.records() {
            if record.get("event").and_then(serde_json::Value::as_str) == Some("command") {
                assert_ne!(mode, ProcessingMode::Segments);
                assert_eq!(
                    record.get("tool").and_then(serde_json::Value::as_str),
                    Some("packager")
                );
            }
        }
    }
    assert_eq!(std::fs::read(&input).unwrap(), original);
}

#[test]
fn public_aes_helper_keeps_the_historical_saved_two_second_selection() {
    let root = crate::native::private_tempdir().unwrap();
    let input = source(&root.path().join("source"));
    let events = Arc::new(Collector::default());
    let command = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
        "packager",
        "must-not-run-decoder",
    )
    .with_ffprobe("must-not-run-probe")
    .with_saved_processing(None);
    assert_eq!(command.segment_duration_ms, Some(2000));
    let key = HlsOutputKey::new(
        "https://synthetic.invalid/output",
        Aes128Key::from_hex(&"31".repeat(16)).unwrap(),
    )
    .unwrap();
    let output = root.path().join("legacy");
    let converted = command
        .convert_hls_to_hls_aes128(
            &input,
            &output,
            &InputKeys::default(),
            &HlsInputKeys::default(),
            &key,
        )
        .unwrap();
    assert!(converted.verification.passed);
    let playlist = output.join("media.m3u8");
    let text = std::fs::read_to_string(&playlist).unwrap();
    let target: u64 = text
        .lines()
        .find_map(|line| line.strip_prefix("#EXT-X-TARGETDURATION:"))
        .unwrap()
        .parse()
        .unwrap();
    assert!((2..=3).contains(&target));
    let durations = text
        .lines()
        .filter_map(|line| line.strip_prefix("#EXTINF:"))
        .map(|value| value.split(',').next().unwrap().parse::<f64>().unwrap())
        .collect::<Vec<_>>();
    assert!(durations.iter().all(|duration| *duration < 2.5));
    assert!(durations.iter().any(|duration| *duration > 1.9));
    let output_keys = HlsInputKeys::new(vec![(
        key.key_uri().into(),
        Aes128Key::from_hex(&"31".repeat(16)).unwrap(),
    )])
    .unwrap();
    assert!(
        command
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &input,
                    input_keys: &InputKeys::default(),
                    hls_keys: &HlsInputKeys::default()
                },
                output: VerificationSource {
                    input: &playlist,
                    input_keys: &InputKeys::default(),
                    hls_keys: &output_keys
                },
            })
            .unwrap()
            .passed
    );
    for record in events.records() {
        if record.get("event").and_then(serde_json::Value::as_str) == Some("command") {
            assert_eq!(
                record.get("tool").and_then(serde_json::Value::as_str),
                Some("packager")
            );
        }
    }
}
