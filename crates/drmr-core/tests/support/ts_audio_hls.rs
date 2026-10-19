//! Audio-only transport to fragmented HLS retains the complete original presentation.
use super::*;

fn fmp4_hls_audio_row(
    layout: &str,
    input_label: &str,
    source: &Path,
    input_keys: &HlsInputKeys,
    clear: &Path,
    root: &Path,
    counts: (Option<u64>, Option<u64>),
) {
    use drmr_core::conversion::{ContentKey, OutputKey, VerificationRequest, VerificationSource};
    assert_eq!(layout, "audio");
    assert_eq!(counts, (None, Some(470)));
    let empty = InputKeys::default();
    let empty_hls = HlsInputKeys::default();
    let events = Arc::new(drmr_core::events::Collector::default());
    let c = Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            events.clone(),
        )),
        "packager",
        "ffmpeg",
    );
    let before = common::snapshot_tree(source.parent().unwrap());
    let output_kid = "909192939495969798999a9b9c9d9e9f";
    for target in [
        TargetProtection::Clear,
        TargetProtection::Cbcs,
        TargetProtection::HlsAes128,
    ] {
        let output = root.join(format!("fmp4-hls-{input_label}-{}", target.as_str()));
        let key = (target == TargetProtection::Cbcs)
            .then(|| OutputKey::from_hex(output_kid, OUTPUT_KEY).unwrap());
        let aes = (target == TargetProtection::HlsAes128)
            .then(|| HlsOutputKey::new(KEY_URI, Aes128Key::from_hex(OUTPUT_KEY).unwrap()).unwrap());
        let result = c
            .convert(ConversionRequest {
                input: source,
                output: &output,
                format: OutputFormat::Hls,
                target,
                input_keys: &empty,
                hls_input_keys: input_keys,
                output_key: key.as_ref(),
                hls_key_uri: key.as_ref().map(|_| KEY_URI),
                hls_output_key: aes.as_ref(),
            })
            .unwrap_or_else(|error| {
                panic!(
                    "audio/{input_label}/{} fMP4 HLS: {}",
                    target.as_str(),
                    error.0
                )
            });
        all_checks(&result.verification);
        assert_fmp4_hls_layout(&output, &result);
        let result_keys = if key.is_some() {
            InputKeys::new(vec![ContentKey::from_hex(output_kid, OUTPUT_KEY).unwrap()]).unwrap()
        } else {
            InputKeys::default()
        };
        let result_hls = if aes.is_some() {
            HlsInputKeys::new(vec![(
                KEY_URI.into(),
                Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
            )])
            .unwrap()
        } else {
            HlsInputKeys::default()
        };
        for (original, keys) in [(source, input_keys), (clear, &empty_hls)] {
            all_checks(
                &c.verify(VerificationRequest {
                    source: VerificationSource {
                        input: original,
                        input_keys: &empty,
                        hls_keys: keys,
                    },
                    output: VerificationSource {
                        input: &output.join("master.m3u8"),
                        input_keys: &result_keys,
                        hls_keys: &result_hls,
                    },
                })
                .unwrap_or_else(|error| {
                    panic!(
                        "audio/{input_label}/{} fresh fMP4 HLS: {}",
                        target.as_str(),
                        error.0
                    )
                }),
            );
        }
        assert_eq!(common::snapshot_tree(source.parent().unwrap()), before);
        assert_private_report(&events, &result);
        println!(
            "DRMR_TS_AUDIO_FMP4_HLS_CELL {}",
            serde_json::json!({
                "input": input_label, "target": target.as_str(), "native_bound_audio_frames": 470,
                "stream_metadata": true, "packet_payloads_equal": true, "packet_timing_equal": true,
                "decoded_content_equal": true, "fresh_verification": true, "source_immutable": true,
                "license_playback_verified": false
            })
        );
    }
}

#[test]
fn audio_nine_fmp4_hls_protection_routes_preserve_every_original_frame_and_sample() {
    matrix_with("audio", fmp4_hls_audio_row);
}

fn clear_request<'a>(
    source: &'a Path,
    output: &'a Path,
    empty: &'a InputKeys,
    hls: &'a HlsInputKeys,
) -> ConversionRequest<'a> {
    ConversionRequest {
        input: source,
        output,
        format: OutputFormat::Hls,
        target: TargetProtection::Clear,
        input_keys: empty,
        hls_input_keys: hls,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    }
}

fn verify_clear(
    source: &Path,
    result: &Path,
) -> Result<drmr_core::conversion::VerificationReport, drmr_core::conversion::ConversionError> {
    use drmr_core::conversion::{VerificationRequest, VerificationSource};
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    converter().verify(VerificationRequest {
        source: VerificationSource {
            input: source,
            input_keys: &empty,
            hls_keys: &hls,
        },
        output: VerificationSource {
            input: result,
            input_keys: &empty,
            hls_keys: &hls,
        },
    })
}

fn join_resources(playlist: &Path, destination: &Path, init: bool) {
    use std::io::Write as _;
    let mut file = drmr_core::native::create_private_new(destination).unwrap();
    if init {
        file.write_all(&std::fs::read(playlist.parent().unwrap().join("init.mp4")).unwrap())
            .unwrap();
    }
    for line in std::fs::read_to_string(playlist)
        .unwrap()
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
    {
        assert!(!line.contains(['/', '\\', ':']));
        file.write_all(&std::fs::read(playlist.parent().unwrap().join(line)).unwrap())
            .unwrap();
    }
}

fn packet_clocks(path: &Path) -> Vec<[num_rational::Ratio<i64>; 3]> {
    let output = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-threads",
            "1",
            "-select_streams",
            "a:0",
            "-show_packets",
            "-show_entries",
            "packet=pts,dts,duration:stream=codec_type,time_base",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let clock = value["streams"][0]["time_base"].as_str().unwrap();
    let (n, d) = clock.split_once('/').unwrap();
    let scale = num_rational::Ratio::new(n.parse::<i64>().unwrap(), d.parse::<i64>().unwrap());
    value["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| ["pts", "dts", "duration"].map(|field| scale * p[field].as_i64().unwrap()))
        .collect()
}

fn decoded_audio(path: &Path) -> (u64, String) {
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
        "pcm_s32le".into(),
        "-f".into(),
        "s32le".into(),
        "-".into(),
    ];
    let runner = ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink));
    let result = runner
        .run_tool(
            &argv,
            "ffmpeg",
            &drmr_core::toolrun::RunOptions {
                timeout: Some(std::time::Duration::from_secs(30)),
                max_stdout_bytes: 470 * 1024 * 2 * 4,
                max_output_chars: 0,
                log_stdout: false,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.returncode, Some(0));
    assert_eq!(result.stdout.len() % 8, 0);
    (
        u64::try_from(result.stdout.len() / 8).unwrap(),
        hex::encode(Sha256::digest(&result.stdout)),
    )
}

#[test]
fn audio_fmp4_hls_keeps_absolute_packet_end_and_all_481280_decoded_samples() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let (source, _) = audio_sources(root.path());
    let before = common::snapshot_tree(source.parent().unwrap());
    let output = root.path().join("output");
    all_checks(
        &converter()
            .convert(clear_request(
                &source,
                &output,
                &InputKeys::default(),
                &HlsInputKeys::default(),
            ))
            .unwrap()
            .verification,
    );
    let original = root.path().join("original.ts");
    let candidate = root.path().join("result.mp4");
    join_resources(&source, &original, false);
    join_resources(&output.join("track-0000/media.m3u8"), &candidate, true);
    let a = packet_clocks(&original);
    let b = packet_clocks(&candidate);
    assert_eq!(a.len(), 470);
    assert_eq!(a, b);
    assert!(a[0][0] > num_rational::Ratio::from_integer(0));
    assert_eq!(
        a.last().unwrap()[0] + a.last().unwrap()[2] - a[0][0],
        num_rational::Ratio::new(470 * 1024, 48000)
    );
    let decoded = decoded_audio(&original);
    assert_eq!(decoded.0, 481_280);
    assert_eq!(decoded, decoded_audio(&candidate));
    println!(
        "DRMR_TS_AUDIO_FMP4_HLS_ORACLE {}",
        serde_json::json!({
            "packets": a.len(), "decoded_samples_per_channel": decoded.0,
            "first_pts": a[0][0].to_string(),
            "end_pts": (a.last().unwrap()[0] + a.last().unwrap()[2]).to_string(),
            "decoded_sha256": decoded.1,
        })
    );
    all_checks(&verify_clear(&source, &output.join("master.m3u8")).unwrap());
    assert_eq!(common::snapshot_tree(source.parent().unwrap()), before);
}

fn refused(
    result: Result<
        drmr_core::conversion::VerificationReport,
        drmr_core::conversion::ConversionError,
    >,
) {
    if let Ok(report) = result {
        assert!(!report.passed && !report.media_verified);
    }
}

#[test]
fn audio_fmp4_hls_rejects_changed_duration_end_resources_payload_and_decode_clock() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let (source, _) = audio_sources(root.path());
    let before = common::snapshot_tree(source.parent().unwrap());
    let output = root.path().join("output");
    all_checks(
        &converter()
            .convert(clear_request(
                &source,
                &output,
                &InputKeys::default(),
                &HlsInputKeys::default(),
            ))
            .unwrap()
            .verification,
    );
    let output_before = common::snapshot_tree(&output);
    let result = output.join("master.m3u8");
    let playlist = output.join("track-0000/media.m3u8");
    let original = std::fs::read_to_string(&playlist).unwrap();
    let lines: Vec<_> = original.lines().collect();
    let last = lines
        .iter()
        .rposition(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap();
    assert!(lines[last - 1].starts_with("#EXTINF:"));
    let mut missing = lines.clone();
    missing.drain(last - 1..=last);
    let mut repeated = lines.clone();
    let insertion = last + 1;
    repeated.splice(insertion..insertion, [lines[last - 1], lines[last]]);
    let duration = lines[last - 1]
        .strip_prefix("#EXTINF:")
        .unwrap()
        .trim_end_matches(',')
        .parse::<f64>()
        .unwrap();
    for (label, changed) in [
        (
            "duration",
            original.replace(
                lines[last - 1],
                &format!("#EXTINF:{},", duration + 1024.0 / 48000.0),
            ),
        ),
        ("end", original.replace("#EXT-X-ENDLIST", "")),
        ("missing-segment", missing.join("\n") + "\n"),
        ("repeated-segment", repeated.join("\n") + "\n"),
    ] {
        std::fs::write(&playlist, changed).unwrap();
        refused(verify_clear(&source, &result));
        std::fs::write(&playlist, &original).unwrap();
        all_checks(&verify_clear(&source, &result).unwrap());
        println!("audio-fmp4-hls-negative: {label}");
    }
    let last_segment = playlist.parent().unwrap().join(lines[last]);
    let bytes = std::fs::read(&last_segment).unwrap();
    let tfdt = bytes.windows(4).position(|w| w == b"tfdt").unwrap();
    let mut shifted = bytes.clone();
    if bytes[tfdt + 4] == 1 {
        let at = tfdt + 8;
        let clock = u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap());
        shifted[at..at + 8].copy_from_slice(&(clock + 1024).to_be_bytes());
    } else {
        let at = tfdt + 8;
        let clock = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
        shifted[at..at + 4].copy_from_slice(&(clock + 1024).to_be_bytes());
    }
    let mut corrupt = bytes.clone();
    let mdat = bytes.windows(4).position(|w| w == b"mdat").unwrap();
    corrupt[mdat + 4] ^= 1;
    for (label, changed) in [("decode-clock", shifted), ("payload", corrupt)] {
        std::fs::write(&last_segment, changed).unwrap();
        refused(verify_clear(&source, &result));
        std::fs::write(&last_segment, &bytes).unwrap();
        all_checks(&verify_clear(&source, &result).unwrap());
        println!("audio-fmp4-hls-negative: {label}");
    }
    assert_eq!(common::snapshot_tree(source.parent().unwrap()), before);
    assert_eq!(common::snapshot_tree(&output), output_before);
}

#[test]
fn audio_fmp4_hls_rejects_wrong_key_domains_and_noncontiguous_original_sample_aes_before_publication()
 {
    use drmr_core::conversion::{ContentKey, OutputKey, VerificationRequest, VerificationSource};
    let root = drmr_core::native::private_tempdir().unwrap();
    let (source, sample) = audio_sources(root.path());
    let source_before = common::snapshot_tree(source.parent().unwrap());
    let aes = aes_source(&source, &root.path().join("aes"));
    let empty = InputKeys::default();
    let no_hls = HlsInputKeys::default();
    let wrong = HlsInputKeys::new_with_ivs(vec![(
        KEY_URI.into(),
        Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
        Some(SegmentIv::from_hex(INPUT_IV).unwrap()),
    )])
    .unwrap();
    for (label, protected) in [("aes", &aes), ("sample", &sample)] {
        let before = common::snapshot_tree(protected.parent().unwrap());
        let output = root.path().join(format!("refused-{label}"));
        assert!(
            converter()
                .convert(clear_request(protected, &output, &empty, &wrong))
                .is_err()
        );
        assert!(!output.exists());
        assert_eq!(common::snapshot_tree(protected.parent().unwrap()), before);
    }
    let key = OutputKey::from_hex("909192939495969798999a9b9c9d9e9f", OUTPUT_KEY).unwrap();
    for target in [TargetProtection::Cbcs, TargetProtection::HlsAes128] {
        let output = root.path().join(format!("key-domain-{}", target.as_str()));
        let aes_key = HlsOutputKey::new(KEY_URI, Aes128Key::from_hex(OUTPUT_KEY).unwrap()).unwrap();
        let mut request = clear_request(&source, &output, &empty, &no_hls);
        request.target = target;
        request.output_key = (target == TargetProtection::Cbcs).then_some(&key);
        request.hls_key_uri = (target == TargetProtection::Cbcs).then_some(KEY_URI);
        request.hls_output_key = (target == TargetProtection::HlsAes128).then_some(&aes_key);
        all_checks(&converter().convert(request).unwrap().verification);
        let wrong_content = InputKeys::new(vec![
            ContentKey::from_hex("909192939495969798999a9b9c9d9e9f", INPUT_KEY).unwrap(),
        ])
        .unwrap();
        let wrong_hls = HlsInputKeys::new(vec![(
            KEY_URI.into(),
            Aes128Key::from_hex(INPUT_KEY).unwrap(),
        )])
        .unwrap();
        let output_before = common::snapshot_tree(&output);
        refused(converter().verify(VerificationRequest {
            source: VerificationSource {
                input: &source,
                input_keys: &empty,
                hls_keys: &no_hls,
            },
            output: VerificationSource {
                input: &output.join("master.m3u8"),
                input_keys: &wrong_content,
                hls_keys: &wrong_hls,
            },
        }));
        assert_eq!(common::snapshot_tree(&output), output_before);
    }
    assert_eq!(
        common::snapshot_tree(source.parent().unwrap()),
        source_before
    );
    let playlist = std::fs::read_to_string(&sample).unwrap();
    let last = playlist
        .lines()
        .rfind(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap();
    super::negatives::shift_final_audio_pes(&sample.parent().unwrap().join(last));
    let before = common::snapshot_tree(sample.parent().unwrap());
    let correct = HlsInputKeys::new_with_ivs(vec![(
        KEY_URI.into(),
        Aes128Key::from_hex(INPUT_KEY).unwrap(),
        Some(SegmentIv::from_hex(INPUT_IV).unwrap()),
    )])
    .unwrap();
    let output = root.path().join("noncontiguous-refused");
    assert!(
        converter()
            .convert(clear_request(&sample, &output, &empty, &correct))
            .is_err()
    );
    assert!(!output.exists());
    assert_eq!(common::snapshot_tree(sample.parent().unwrap()), before);
}

fn assert_fmp4_hls_layout(output: &Path, result: &drmr_core::conversion::ConversionReport) {
    assert_eq!(result.output.tracks.len(), 1);
    assert_eq!(
        result.output.tracks[0].kind,
        drmr_core::inspection::TrackKind::Audio
    );

    let master = std::fs::read_to_string(output.join("master.m3u8")).unwrap();
    assert_eq!(master.matches("#EXT-X-STREAM-INF:").count(), 1);
    assert!(!master.contains("#EXT-X-MEDIA:"));
    assert!(master.contains("CODECS=\"mp4a.40.2\""));
    assert_eq!(
        master
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect::<Vec<_>>(),
        ["track-0000/media.m3u8"]
    );
    let media = std::fs::read_to_string(output.join("track-0000/media.m3u8")).unwrap();
    assert_eq!(media.matches("#EXT-X-MAP:").count(), 1);
    assert_eq!(media.matches("#EXT-X-ENDLIST").count(), 1);
    assert!(media.contains("#EXT-X-PLAYLIST-TYPE:VOD"));
    let segments: Vec<_> = media
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    assert!(segments.len() > 1);
    assert_eq!(segments.len(), media.matches("#EXTINF:").count());
    assert!(output.join("track-0000/init.mp4").is_file());
    for name in segments {
        assert!(!name.contains(['/', '\\', ':']));
        assert_eq!(
            Path::new(name).extension(),
            Some(std::ffi::OsStr::new("m4s"))
        );
        assert!(output.join("track-0000").join(name).is_file());
    }
}

fn assert_private_report(
    events: &drmr_core::events::Collector,
    result: &drmr_core::conversion::ConversionReport,
) {
    let shown = format!(
        "{}{}",
        serde_json::to_string(&events.records()).unwrap(),
        serde_json::to_string(&result).unwrap()
    );
    for secret in [INPUT_KEY, INPUT_IV, OUTPUT_KEY, OUTPUT_IV, KEY_URI] {
        assert!(
            !shown.contains(secret),
            "operator material entered reports/events"
        );
    }
}
