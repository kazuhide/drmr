//! Independent media oracles for AAC whose frame clock is fractional in MPEG-TS.
use super::audio_hls::{decoded_audio, join_resources, packet_clocks};
use super::*;

fn encoded_audio(path: &Path) -> (usize, String) {
    use sha2::{Digest as _, Sha256};
    let argv = vec![
        "ffmpeg".into(),
        "-v".into(),
        "error".into(),
        "-nostdin".into(),
        "-threads".into(),
        "1".into(),
        "-i".into(),
        drmr_core::manifest_support::path::fspath(path).unwrap(),
        "-map".into(),
        "0:a:0".into(),
        "-c:a".into(),
        "copy".into(),
        "-f".into(),
        "adts".into(),
        "-".into(),
    ];
    let runner = ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink));
    let result = runner
        .run_tool(
            &argv,
            "ffmpeg",
            &drmr_core::toolrun::RunOptions {
                timeout: Some(std::time::Duration::from_secs(30)),
                max_stdout_bytes: 1024 * 1024,
                max_output_chars: 0,
                log_stdout: false,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.returncode, Some(0));
    let mut at = 0;
    let mut count = 0;
    while at < result.stdout.len() {
        let head = &result.stdout[at..at + 7];
        assert_eq!(head[0], 0xff);
        assert_eq!(head[1] & 0xf6, 0xf0);
        assert_eq!(head[2] >> 6, 1, "AAC Low Complexity profile is required");
        assert_eq!((head[2] >> 2) & 15, 4, "44.1 kHz configuration changed");
        assert_eq!(((head[2] & 1) << 2) | (head[3] >> 6), 2);
        assert_eq!(head[6] & 3, 0, "one raw data block per frame is required");
        let length = (usize::from(head[3] & 3) << 11)
            | (usize::from(head[4]) << 3)
            | usize::from(head[5] >> 5);
        assert!(length > 7 && at + length <= result.stdout.len());
        at += length;
        count += 1;
    }
    assert_eq!(at, result.stdout.len());
    assert!(count > 0);
    (count, hex::encode(Sha256::digest(&result.stdout)))
}

fn join_dash_track(output: &Path, destination: &Path) {
    use std::io::Write as _;
    let directory = output.join("track-0000");
    let mut segments: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "m4s"))
        .collect();
    segments.sort_by_key(|path| {
        path.file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("seg_")
            .unwrap()
            .parse::<usize>()
            .unwrap()
    });
    assert!(segments.len() > 1);
    let mut file = drmr_core::native::create_private_new(destination).unwrap();
    file.write_all(&std::fs::read(directory.join("init.mp4")).unwrap())
        .unwrap();
    for segment in segments {
        file.write_all(&std::fs::read(segment).unwrap()).unwrap();
    }
}

fn assert_original_packet_clock(
    source: &[[num_rational::Ratio<i64>; 3]],
    candidate: &[[num_rational::Ratio<i64>; 3]],
) {
    use num_rational::Ratio;
    assert_ne!(source.len(), 0);
    assert_eq!(candidate.len(), source.len());
    let nominal_duration = Ratio::new((1024 * 90_000) / 44_100, 90_000);
    assert_eq!(nominal_duration, Ratio::new(2089, 90_000));
    for (index, (original, result)) in source.iter().zip(candidate).enumerate() {
        assert_eq!(original[0], original[1]);
        assert_eq!(original[2], nominal_duration);
        assert_eq!(result[0], original[0], "frame {index} presentation clock");
        assert_eq!(result[1], original[1], "frame {index} decoding clock");
        // A TS probe reports the nominal truncated frame duration. The MP4
        // timeline stores the exact observed distance to the following frame.
        let duration = source
            .get(index + 1)
            .map_or(nominal_duration, |next| next[1] - original[1]);
        assert!(duration > Ratio::from_integer(0));
        assert_eq!(result[2], duration, "frame {index} MP4 sample duration");
    }
    assert!(source[0][0] > Ratio::from_integer(0));
}

#[test]
fn audio_44100_all_output_containers_preserve_every_original_aac_frame_and_pcm_sample() {
    use drmr_core::conversion::{VerificationRequest, VerificationSource};
    let root = drmr_core::native::private_tempdir().unwrap();
    let (source, _) = audio_sources_at_rate(root.path(), Some(44_100));
    let before = common::snapshot_tree(source.parent().unwrap());
    let original = root.path().join("original.ts");
    join_resources(&source, &original, false);
    let source_packets = packet_clocks(&original);
    let encoded = encoded_audio(&original);
    let decoded = decoded_audio(&original);
    assert_eq!(encoded.0, source_packets.len());
    assert_eq!(decoded.0, u64::try_from(encoded.0).unwrap() * 1024);
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let c = converter();
    for (format, name, manifest) in [
        (OutputFormat::Hls, "hls", "master.m3u8"),
        (OutputFormat::Dash, "dash", "manifest.mpd"),
        (OutputFormat::HlsTs, "hls-ts", "master.m3u8"),
    ] {
        let output = root.path().join(name);
        let result = c
            .convert(ConversionRequest {
                input: &source,
                output: &output,
                format,
                target: TargetProtection::Clear,
                input_keys: &empty,
                hls_input_keys: &hls,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap_or_else(|error| panic!("44.1 kHz audio-only {name}: {}", error.0));
        all_checks(&result.verification);
        let candidate = root.path().join(format!(
            "{name}.{}",
            if format == OutputFormat::HlsTs {
                "ts"
            } else {
                "mp4"
            }
        ));
        if format == OutputFormat::Hls {
            join_resources(&output.join("track-0000/media.m3u8"), &candidate, true);
        } else if format == OutputFormat::HlsTs {
            join_resources(&output.join("track-0000/media.m3u8"), &candidate, false);
        } else {
            join_dash_track(&output, &candidate);
        }
        let candidate_packets = packet_clocks(&candidate);
        if format == OutputFormat::HlsTs {
            assert_eq!(candidate_packets, source_packets);
        } else {
            assert_original_packet_clock(&source_packets, &candidate_packets);
        }
        assert_eq!(encoded_audio(&candidate), encoded);
        assert_eq!(decoded_audio(&candidate), decoded);
        all_checks(
            &c.verify(VerificationRequest {
                source: VerificationSource {
                    input: &source,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
                output: VerificationSource {
                    input: &output.join(manifest),
                    input_keys: &empty,
                    hls_keys: &hls,
                },
            })
            .unwrap(),
        );
        assert_eq!(common::snapshot_tree(source.parent().unwrap()), before);
        println!(
            "DRMR_TS_AAC_44100_ORACLE {}",
            serde_json::json!({
                "output": name, "sample_rate": 44100, "aac_frames": encoded.0,
                "decoded_samples_per_channel": decoded.0, "encoded_sha256": encoded.1,
                "decoded_sha256": decoded.1, "fresh_verification": true,
                "original_pts_dts_equal": true,
                "mp4_durations_follow_original_packet_clock": format != OutputFormat::HlsTs,
                "ts_nominal_durations_equal": format == OutputFormat::HlsTs,
                "first_pts": source_packets[0][0].to_string(),
                "end_pts": (candidate_packets.last().unwrap()[0]
                    + candidate_packets.last().unwrap()[2]).to_string(),
                "source_immutable": true, "license_playback_verified": false,
            })
        );
    }
}

fn rewrite_sample_span_durations(source: &Path) -> Vec<u32> {
    let mut lines: Vec<String> = std::fs::read_to_string(source)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let runner = ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink));
    let mut frames = Vec::new();
    let mut duration_at = None;
    for index in 0..lines.len() {
        let line = &lines[index];
        if line.starts_with("#EXTINF:") {
            assert!(duration_at.replace(index).is_none());
        } else if !line.is_empty() && !line.starts_with('#') {
            assert!(!line.contains(['/', '\\', ':']));
            let segment = source.parent().unwrap().join(line);
            let argv = vec![
                "ffprobe".into(),
                "-v".into(),
                "error".into(),
                "-threads".into(),
                "1".into(),
                "-select_streams".into(),
                "a:0".into(),
                "-count_packets".into(),
                "-show_entries".into(),
                "stream=codec_name,sample_rate,channels,nb_read_packets".into(),
                "-of".into(),
                "json".into(),
                drmr_core::manifest_support::path::fspath(&segment).unwrap(),
            ];
            let result = runner
                .run_tool(
                    &argv,
                    "ffprobe",
                    &drmr_core::toolrun::RunOptions {
                        timeout: Some(std::time::Duration::from_secs(30)),
                        max_stdout_bytes: 65536,
                        max_output_chars: 0,
                        log_stdout: false,
                        ..Default::default()
                    },
                )
                .unwrap();
            assert_eq!(result.returncode, Some(0));
            let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            let streams = report["streams"].as_array().unwrap();
            assert_eq!(streams.len(), 1);
            let stream = &streams[0];
            assert_eq!(stream["codec_name"], "aac");
            assert_eq!(stream["sample_rate"], "44100");
            assert_eq!(stream["channels"], 2);
            let count: u32 = stream["nb_read_packets"].as_str().unwrap().parse().unwrap();
            assert!(count > 0);
            let seconds = f64::from(count.checked_mul(1024).unwrap()) / 44_100.0;
            let declared = seconds.to_string();
            assert!(declared.split_once('.').unwrap().1.len() > 9);
            assert_eq!(
                declared.parse::<f64>().unwrap().to_bits(),
                seconds.to_bits()
            );
            lines[duration_at.take().unwrap()] = format!("#EXTINF:{declared},");
            frames.push(count);
        }
    }
    assert!(duration_at.is_none());
    assert_eq!(frames.len(), 6);
    std::fs::write(source, lines.join("\n") + "\n").unwrap();
    frames
}

#[test]
fn audio_44100_long_sample_span_extinf_passes_analysis_saved_plans_conversion_and_fresh_verification()
 {
    use drmr_core::conversion::{
        VerificationSource,
        key_file::OperatorKeys,
        plan::{self, ExecutionPlanRequest},
    };
    let root = drmr_core::native::private_tempdir().unwrap();
    let (source, _) = audio_sources_at_rate(root.path(), Some(44_100));
    let frame_counts = rewrite_sample_span_durations(&source);
    assert_eq!(frame_counts.iter().sum::<u32>(), 432);
    let before = common::snapshot_tree(source.parent().unwrap());
    let original = root.path().join("original.ts");
    join_resources(&source, &original, false);
    let source_packets = packet_clocks(&original);
    let encoded = encoded_audio(&original);
    let decoded = decoded_audio(&original);
    assert_eq!(encoded.0, 432);
    assert_eq!(source_packets.len(), encoded.0);
    assert_eq!(decoded.0, 432 * 1024);
    let keys = OperatorKeys::default();
    let c = converter();
    let analysis = c
        .analyze_source(VerificationSource {
            input: &source,
            input_keys: &keys.input,
            hls_keys: &keys.hls_input,
        })
        .unwrap();
    assert!(analysis.input_admitted);
    assert_eq!(analysis.actual_format, OutputFormat::HlsTs);
    assert!(analysis.verification.checks.stream_metadata.is_pass());
    assert!(analysis.verification.checks.packet_payloads_equal);
    assert!(analysis.verification.checks.packet_timing_equal);
    assert!(analysis.verification.decode_evidence.satisfies(
        drmr_core::conversion::ValidationDepth::Decoded,
        analysis.verification.scope
    ));
    for (format, name) in [(OutputFormat::Hls, "hls"), (OutputFormat::Dash, "dash")] {
        let output = root.path().join(name);
        let planned = c
            .create_execution_plan(ExecutionPlanRequest {
                input: VerificationSource {
                    input: &source,
                    input_keys: &keys.input,
                    hls_keys: &keys.hls_input,
                },
                output: &output,
                format: Some(format),
                target: TargetProtection::Clear,
            })
            .unwrap();
        assert_eq!(planned.source, analysis.source);
        assert_eq!(planned.source_format, OutputFormat::HlsTs);
        let saved = root.path().join(format!("{name}.plan.json"));
        plan::write_new(&saved, &planned).unwrap();
        let saved_bytes = std::fs::read(&saved).unwrap();
        let loaded = plan::read(&saved).unwrap();
        assert_eq!(loaded.plan_id, planned.plan_id);
        all_checks(&c.execute_plan(&loaded, &keys).unwrap().verification);
        let verified = converter().verify_plan(&loaded, &keys).unwrap();
        assert_eq!(verified.plan_id, planned.plan_id);
        assert!(verified.source_unchanged && verified.output_matches_plan);
        all_checks(&verified.verification);
        let candidate = root.path().join(format!("{name}.mp4"));
        if format == OutputFormat::Hls {
            join_resources(&output.join("track-0000/media.m3u8"), &candidate, true);
        } else {
            join_dash_track(&output, &candidate);
        }
        assert_original_packet_clock(&source_packets, &packet_clocks(&candidate));
        assert_eq!(encoded_audio(&candidate), encoded);
        assert_eq!(decoded_audio(&candidate), decoded);
        assert_eq!(std::fs::read(&saved).unwrap(), saved_bytes);
        assert_eq!(common::snapshot_tree(source.parent().unwrap()), before);
    }
}

fn matrix_44100(row: MatrixRow) {
    let root = drmr_core::native::private_tempdir().unwrap();
    let (clear, sample) = audio_sources_at_rate(root.path(), Some(44_100));
    let counts = original_counts(root.path(), &clear);
    assert_eq!(counts.0, None);
    assert!(counts.1.is_some_and(|count| count > 0));
    let aes = aes_source(&clear, &root.path().join("aes"));
    let encrypted_keys = HlsInputKeys::new_with_ivs(vec![(
        KEY_URI.into(),
        Aes128Key::from_hex(INPUT_KEY).unwrap(),
        Some(SegmentIv::from_hex(INPUT_IV).unwrap()),
    )])
    .unwrap();
    let empty_hls = HlsInputKeys::default();
    for (input_label, source, input_keys) in [
        ("clear", &clear, &empty_hls),
        ("aes-128", &aes, &encrypted_keys),
        ("sample-aes", &sample, &encrypted_keys),
    ] {
        row(
            "audio",
            input_label,
            source,
            input_keys,
            &clear,
            root.path(),
            counts,
        );
    }
}

#[test]
fn audio_44100_eighteen_fmp4_hls_and_dash_protection_routes_preserve_original_media() {
    matrix_44100(|layout, label, source, keys, clear, root, counts| {
        super::audio_hls::fmp4_hls_audio_row(layout, label, source, keys, clear, root, counts);
        dash_row(layout, label, source, keys, clear, root, counts);
    });
}

#[test]
fn audio_44100_nine_ts_protection_routes_preserve_original_media() {
    matrix_44100(matrix_row);
}

#[test]
fn audio_44100_refuses_shifted_source_pes_and_changed_aac_configuration() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let (source, _) = audio_sources_at_rate(root.path(), Some(44_100));
    let original = std::fs::read_to_string(&source).unwrap();
    let last = original
        .lines()
        .rfind(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap();
    let segment = source.parent().unwrap().join(last);
    let bytes = std::fs::read(&segment).unwrap();
    let source_before = common::snapshot_tree(source.parent().unwrap());
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    for (mutation, expected) in [
        ("pes-clock", "ts-clock-wrap-or-discontinuity-not-supported"),
        ("configuration", "ts-codec-configuration-not-supported"),
    ] {
        if mutation == "pes-clock" {
            // One AAC frame spans 307200/147 TS ticks. A whole-frame shift cannot
            // be explained by the unknown phase used to quantize a TS timestamp.
            super::negatives::shift_final_audio_pes_by(&segment, 2090);
        } else {
            super::negatives::change_final_audio_sample_rate(&segment);
        }
        let mutated = common::snapshot_tree(source.parent().unwrap());
        for (format, name) in [(OutputFormat::Hls, "hls"), (OutputFormat::Dash, "dash")] {
            let output = root.path().join(format!("refused-{mutation}-{name}"));
            let error = converter()
                .convert(ConversionRequest {
                    input: &source,
                    output: &output,
                    format,
                    target: TargetProtection::Clear,
                    input_keys: &empty,
                    hls_input_keys: &hls,
                    output_key: None,
                    hls_key_uri: None,
                    hls_output_key: None,
                })
                .unwrap_err();
            assert_eq!(error.0, expected, "44.1 kHz {mutation} {name}");
            assert!(!output.exists());
            assert_eq!(common::snapshot_tree(source.parent().unwrap()), mutated);
        }
        std::fs::write(&segment, &bytes).unwrap();
    }
    assert_eq!(
        common::snapshot_tree(source.parent().unwrap()),
        source_before
    );
}
