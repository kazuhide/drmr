//! Audio-only transport to fragmented HLS retains the complete original presentation.
use super::*;

pub(super) fn fmp4_hls_audio_row(
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
    assert_eq!(counts.0, None);
    let audio_frames = counts.1.unwrap();
    assert!(audio_frames > 0);
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
    )
    .with_decode(true);
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
                "input": input_label, "target": target.as_str(), "native_bound_audio_frames": audio_frames,
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
    verify_with_result_keys(
        source,
        result,
        &InputKeys::default(),
        &HlsInputKeys::default(),
    )
}

fn verify_with_result_keys(
    source: &Path,
    result: &Path,
    output_keys: &InputKeys,
    output_hls_keys: &HlsInputKeys,
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
            input_keys: output_keys,
            hls_keys: output_hls_keys,
        },
    })
}

pub(super) fn join_resources(playlist: &Path, destination: &Path, init: bool) {
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

pub(super) fn packet_clocks(path: &Path) -> Vec<[num_rational::Ratio<i64>; 3]> {
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

pub(super) fn decoded_audio(path: &Path) -> (u64, String) {
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

#[track_caller]
fn expected_error(error: drmr_core::conversion::ConversionError, expected: &str) {
    assert_eq!(
        error.0, expected,
        "unexpected failure cannot prove media rejection"
    );
}

#[track_caller]
fn refused(
    result: Result<
        drmr_core::conversion::VerificationReport,
        drmr_core::conversion::ConversionError,
    >,
    expected: &str,
) {
    match result {
        Ok(report) => {
            assert_eq!(report.schema_version, 2);
            assert!(!report.passed);
            assert!(
                !report.checks.is_pass()
                    || !report.decode_evidence.satisfies(
                        drmr_core::conversion::ValidationDepth::Decoded,
                        report.scope
                    )
            );
            assert!(!report.license_playback_verified);
            assert!(
                !report.checks.stream_metadata.is_pass()
                    || !report.checks.packet_payloads_equal
                    || !report.checks.packet_timing_equal
                    || matches!(
                        report.decode_evidence,
                        drmr_core::conversion::DecodeEvidence::ContentCompared { equal: false }
                    )
            );
        }
        Err(error) => expected_error(error, expected),
    }
}

#[test]
fn audio_fmp4_hls_negative_oracle_rejects_operational_and_unknown_failures() {
    use drmr_core::conversion::ConversionError;
    let expected_codes = [
        "manifest-segment-duration-mismatch",
        "hls-complete-vod-required",
        "ts-original-access-unit-count-mismatch",
        "fragment-timeline-gap-or-overlap",
        "media-tool-failed",
        "invalid-encrypted-segment",
        "fragment-timing-not-supported",
        "ts-aac-sample-rate-not-supported",
        "ts-clock-wrap-or-discontinuity-not-supported",
    ];
    for expected in expected_codes {
        for code in [
            "media-tool-timeout",
            "resource-budget",
            "cleanup-unconfirmed",
            "media-cleanup-unconfirmed",
            "media-output-limit",
            "media-tool-unavailable-or-timeout",
            "cancelled",
            "unknown-fixture-error",
        ] {
            assert!(
                std::panic::catch_unwind(|| refused(Err(ConversionError(code)), expected)).is_err(),
                "operational or unknown failure was accepted as media rejection: {code}"
            );
        }
        let wrong_semantic = if expected == "hls-complete-vod-required" {
            "invalid-encrypted-segment"
        } else {
            "hls-complete-vod-required"
        };
        assert!(
            std::panic::catch_unwind(|| refused(Err(ConversionError(wrong_semantic)), expected))
                .is_err()
        );
        refused(Err(ConversionError(expected)), expected);
    }
    assert!(
        std::panic::catch_unwind(|| refused(
            Err(ConversionError("fragment-timing-not-supported")),
            "invalid-encrypted-segment"
        ))
        .is_err()
    );
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
    for (label, expected, changed) in [
        (
            "duration",
            "manifest-segment-duration-mismatch",
            original.replace(
                lines[last - 1],
                &format!("#EXTINF:{},", duration + 1024.0 / 48000.0),
            ),
        ),
        (
            "end",
            "hls-complete-vod-required",
            original.replace("#EXT-X-ENDLIST", ""),
        ),
        (
            "missing-segment",
            "ts-original-access-unit-count-mismatch",
            missing.join("\n") + "\n",
        ),
        (
            "repeated-segment",
            "fragment-timeline-gap-or-overlap",
            repeated.join("\n") + "\n",
        ),
    ] {
        std::fs::write(&playlist, changed).unwrap();
        refused(verify_clear(&source, &result), expected);
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
    // Corrupt payload bytes can make decoding fail before a mismatch report is produced.
    for (label, expected, changed) in [
        ("decode-clock", "fragment-timeline-gap-or-overlap", shifted),
        ("payload", "media-tool-failed", corrupt),
    ] {
        std::fs::write(&last_segment, changed).unwrap();
        refused(verify_clear(&source, &result), expected);
        std::fs::write(&last_segment, &bytes).unwrap();
        all_checks(&verify_clear(&source, &result).unwrap());
        println!("audio-fmp4-hls-negative: {label}");
    }
    assert_eq!(common::snapshot_tree(source.parent().unwrap()), before);
    assert_eq!(common::snapshot_tree(&output), output_before);
}

fn assert_wrong_output_keys_are_rejected(source: &Path, root: &Path) {
    use drmr_core::conversion::{ContentKey, OutputKey};
    let empty = InputKeys::default();
    let no_hls = HlsInputKeys::default();
    let key = OutputKey::from_hex("909192939495969798999a9b9c9d9e9f", OUTPUT_KEY).unwrap();
    for target in [TargetProtection::Cbcs, TargetProtection::HlsAes128] {
        let output = root.join(format!("key-domain-{}", target.as_str()));
        let aes_key = HlsOutputKey::new(KEY_URI, Aes128Key::from_hex(OUTPUT_KEY).unwrap()).unwrap();
        let mut request = clear_request(source, &output, &empty, &no_hls);
        request.target = target;
        request.output_key = (target == TargetProtection::Cbcs).then_some(&key);
        request.hls_key_uri = (target == TargetProtection::Cbcs).then_some(KEY_URI);
        request.hls_output_key = (target == TargetProtection::HlsAes128).then_some(&aes_key);
        all_checks(&converter().convert(request).unwrap().verification);
        let wrong_content = InputKeys::new(vec![
            ContentKey::from_hex("909192939495969798999a9b9c9d9e9f", INPUT_KEY).unwrap(),
        ])
        .unwrap();
        let wrong_aes = if target == TargetProtection::HlsAes128 {
            invalid_padding_key(
                &output.join("track-0000/media.m3u8"),
                OUTPUT_KEY,
                INPUT_KEY,
                "aes-output",
            )
        } else {
            INPUT_KEY.into()
        };
        let wrong_hls = HlsInputKeys::new(vec![(
            KEY_URI.into(),
            Aes128Key::from_hex(&wrong_aes).unwrap(),
        )])
        .unwrap();
        let output_before = common::snapshot_tree(&output);
        let correct_content = InputKeys::new(vec![
            ContentKey::from_hex("909192939495969798999a9b9c9d9e9f", OUTPUT_KEY).unwrap(),
        ])
        .unwrap();
        let correct_hls = HlsInputKeys::new(vec![(
            KEY_URI.into(),
            Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
        )])
        .unwrap();
        let candidate = output.join("master.m3u8");
        all_checks(
            &verify_with_result_keys(source, &candidate, &correct_content, &correct_hls).unwrap(),
        );
        // Wrong CBCS bytes can fail the decoder. Only this observed failure code is accepted;
        // timeouts, cleanup/resource failures and missing tools are never media refusals.
        let expected = if target == TargetProtection::Cbcs {
            "media-tool-failed"
        } else {
            "invalid-encrypted-segment"
        };
        refused(
            verify_with_result_keys(source, &candidate, &wrong_content, &wrong_hls),
            expected,
        );
        all_checks(
            &verify_with_result_keys(source, &candidate, &correct_content, &correct_hls).unwrap(),
        );
        assert_eq!(common::snapshot_tree(&output), output_before);
    }
}

#[test]
fn audio_fmp4_hls_rejects_wrong_key_domains_and_noncontiguous_original_sample_aes_before_publication()
 {
    let root = drmr_core::native::private_tempdir().unwrap();
    let (source, sample) = audio_sources(root.path());
    let source_before = common::snapshot_tree(source.parent().unwrap());
    let aes = aes_source(&source, &root.path().join("aes"));
    let empty = InputKeys::default();
    let wrong_aes = HlsInputKeys::new_with_ivs(vec![(
        KEY_URI.into(),
        Aes128Key::from_hex(&invalid_padding_key(
            &aes,
            INPUT_KEY,
            OUTPUT_KEY,
            "aes-input",
        ))
        .unwrap(),
        Some(SegmentIv::from_hex(INPUT_IV).unwrap()),
    )])
    .unwrap();
    let wrong = HlsInputKeys::new_with_ivs(vec![(
        KEY_URI.into(),
        Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
        Some(SegmentIv::from_hex(INPUT_IV).unwrap()),
    )])
    .unwrap();
    for (label, protected, keys, expected) in [
        ("aes", &aes, &wrong_aes, "invalid-encrypted-segment"),
        (
            "sample",
            &sample,
            &wrong,
            "ts-aac-sample-rate-not-supported",
        ),
    ] {
        let before = common::snapshot_tree(protected.parent().unwrap());
        let output = root.path().join(format!("refused-{label}"));
        let error = converter()
            .convert(clear_request(protected, &output, &empty, keys))
            .unwrap_err();
        expected_error(error, expected);
        assert!(!output.exists());
        assert_eq!(common::snapshot_tree(protected.parent().unwrap()), before);
    }
    assert_wrong_output_keys_are_rejected(&source, root.path());
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
    let error = converter()
        .convert(clear_request(&sample, &output, &empty, &correct))
        .unwrap_err();
    expected_error(error, "ts-clock-wrap-or-discontinuity-not-supported");
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

// Independent test-only CBC block evidence; selection never invokes drmr's decryptor.
struct CbcResource {
    first: [u8; 16],
    previous: [u8; 16],
    last: [u8; 16],
    iv: [u8; 16],
    bytes: u64,
    is_init: bool,
}

fn cbc_plain_block(block: &[u8; 16], previous: &[u8; 16], key: &[u8; 16]) -> [u8; 16] {
    use aes::cipher::{BlockDecrypt as _, KeyInit as _};
    let cipher = aes::Aes128::new_from_slice(key).unwrap();
    let mut plaintext = aes::cipher::Block::<aes::Aes128>::from(*block);
    cipher.decrypt_block(&mut plaintext);
    std::array::from_fn(|i| plaintext[i] ^ previous[i])
}

#[test]
fn audio_fmp4_hls_cbc_tail_oracle_checks_entire_padding_and_chaining() {
    use aes::cipher::{BlockEncrypt as _, KeyInit as _};
    let key: [u8; 16] = hex::decode("2b7e151628aed2a6abf7158809cf4f3c")
        .unwrap()
        .try_into()
        .unwrap();
    let iv: [u8; 16] = hex::decode("000102030405060708090a0b0c0d0e0f")
        .unwrap()
        .try_into()
        .unwrap();
    let block: [u8; 16] = hex::decode("7649abac8119b246cee98e9b12e9197d")
        .unwrap()
        .try_into()
        .unwrap();
    // NIST SP 800-38A F.2.1 anchors both AES decryption and CBC XOR independently.
    assert_eq!(
        hex::encode(cbc_plain_block(&block, &iv, &key)),
        "6bc1bee22e409f96e93d7e117393172a"
    );
    let cipher = aes::Aes128::new_from_slice(&key).unwrap();
    let mut one = [0x41; 16];
    one[15] = 1;
    let mut nonuniform = [0x41; 16];
    nonuniform[15] = 2;
    for (plain, expected) in [
        (one, Some(1)),
        ([16; 16], Some(16)),
        ([0; 16], None),
        ([17; 16], None),
        (nonuniform, None),
    ] {
        let mut block =
            aes::cipher::Block::<aes::Aes128>::from(std::array::from_fn(|i| plain[i] ^ iv[i]));
        cipher.encrypt_block(&mut block);
        let resource = CbcResource {
            first: block.into(),
            previous: iv,
            last: block.into(),
            iv,
            bytes: 16,
            is_init: true,
        };
        assert_eq!(resource.padding(&key), expected);
    }
}

impl CbcResource {
    fn padding(&self, key: &[u8; 16]) -> Option<u8> {
        let last = cbc_plain_block(&self.last, &self.previous, key);
        let padding = last[15];
        ((1..=16).contains(&padding)
            && last[16 - usize::from(padding)..]
                .iter()
                .all(|byte| *byte == padding))
        .then_some(padding)
    }

    fn invalid_first_box(&self, key: &[u8; 16]) -> bool {
        let Some(padding) = self.padding(key) else {
            return false;
        };
        let first = cbc_plain_block(&self.first, &self.iv, key);
        u64::from(u32::from_be_bytes(first[..4].try_into().unwrap()))
            > self.bytes - u64::from(padding)
    }
}

fn cbc_resources(playlist: &Path) -> Vec<CbcResource> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let text = std::fs::read_to_string(playlist).unwrap();
    assert!(text.len() <= 64 * 1024);
    let key_prefix = format!("#EXT-X-KEY:METHOD=AES-128,URI=\"{KEY_URI}\",IV=0x");
    let mut iv = None;
    let mut resources = Vec::new();
    for line in text.lines() {
        if line.starts_with("#EXT-X-KEY:") {
            let value = line.strip_prefix(&key_prefix).expect("fixture key mapping");
            iv = Some(hex::decode(value).unwrap().try_into().unwrap());
            continue;
        }
        let (name, is_init) = if line.starts_with("#EXT-X-MAP:") {
            let value = line.strip_prefix("#EXT-X-MAP:URI=\"").unwrap();
            (value.strip_suffix('"').unwrap(), true)
        } else if !line.is_empty() && !line.starts_with('#') {
            (line, false)
        } else {
            continue;
        };
        assert!(!name.is_empty() && !name.contains(['/', '\\', ':']));
        assert!(resources.len() < 16, "bounded fixture resource inventory");
        let iv: [u8; 16] = iv.expect("encrypted resource must have an explicit fixture IV");
        let mut file = std::fs::File::open(playlist.parent().unwrap().join(name)).unwrap();
        let bytes = file.metadata().unwrap().len();
        assert!((16..=8 * 1024 * 1024).contains(&bytes) && bytes.is_multiple_of(16));
        let mut first = [0; 16];
        file.read_exact(&mut first).unwrap();
        let mut previous = iv;
        let mut last = first;
        if bytes >= 32 {
            file.seek(SeekFrom::End(-32)).unwrap();
            file.read_exact(&mut previous).unwrap();
            file.read_exact(&mut last).unwrap();
        }
        resources.push(CbcResource {
            first,
            previous,
            last,
            iv,
            bytes,
            is_init,
        });
    }
    assert!(
        !resources.is_empty(),
        "padding evidence needs actual resources"
    );
    resources
}

fn select_cbc_key(
    correct: &str,
    preferred: &str,
    predicate: impl Fn(&[u8; 16]) -> bool,
) -> ([u8; 16], u32) {
    let correct: [u8; 16] = hex::decode(correct).unwrap().try_into().unwrap();
    for attempt in 0_u32..65_536 {
        let candidate: [u8; 16] = if attempt == 0 {
            hex::decode(preferred).unwrap().try_into().unwrap()
        } else {
            let mut key = [0xa5; 16];
            key[12..].copy_from_slice(&attempt.to_be_bytes());
            key
        };
        if candidate != correct && predicate(&candidate) {
            return (candidate, attempt);
        }
    }
    panic!("no fixture key satisfies independent padding evidence within the bound");
}

fn invalid_padding_key(playlist: &Path, correct: &str, preferred: &str, stage: &str) -> String {
    let resources = cbc_resources(playlist);
    let correct_key: [u8; 16] = hex::decode(correct).unwrap().try_into().unwrap();
    assert!(
        resources
            .iter()
            .all(|resource| resource.padding(&correct_key).is_some())
    );
    // CBC has no authentication: a wrong key can have valid PKCS#7 padding. Keep the
    // swapped-domain key when possible, otherwise select a demonstrably invalid key
    // for every actual init and segment. No production verification is retried.
    let (wrong, attempt) = select_cbc_key(correct, preferred, |key| {
        resources
            .iter()
            .all(|resource| resource.padding(key).is_none())
    });
    assert!(
        resources
            .iter()
            .all(|resource| resource.padding(&wrong).is_none())
    );
    println!(
        "DRMR_AUDIO_CBC_INVALID_PADDING {}",
        serde_json::json!({
            "stage": stage, "resource_count": resources.len(), "selected_attempt": attempt,
            "all_correct_padding_valid": true, "all_wrong_padding_invalid": true,
        })
    );
    hex::encode(wrong)
}

#[test]
fn audio_fmp4_hls_wrong_aes_key_with_valid_padding_is_rejected_by_fragment_structure() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let (source, _) = audio_sources(root.path());
    let source_before = common::snapshot_tree(source.parent().unwrap());
    let output = root.path().join("valid-padding-control");
    let empty = InputKeys::default();
    let no_hls = HlsInputKeys::default();
    let output_key = HlsOutputKey::new(KEY_URI, Aes128Key::from_hex(OUTPUT_KEY).unwrap()).unwrap();
    let mut request = clear_request(&source, &output, &empty, &no_hls);
    request.target = TargetProtection::HlsAes128;
    request.hls_output_key = Some(&output_key);
    all_checks(&converter().convert(request).unwrap().verification);
    let output_before = common::snapshot_tree(&output);
    let resources = cbc_resources(&output.join("track-0000/media.m3u8"));
    assert!(resources[0].is_init);
    let correct: [u8; 16] = hex::decode(OUTPUT_KEY).unwrap().try_into().unwrap();
    assert!(
        resources
            .iter()
            .all(|resource| resource.padding(&correct).is_some())
    );
    let (wrong, attempt) = select_cbc_key(OUTPUT_KEY, INPUT_KEY, |key| {
        resources[0].padding(key).is_some() && resources[0].invalid_first_box(key)
    });
    let correct_hls = HlsInputKeys::new(vec![(
        KEY_URI.into(),
        Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
    )])
    .unwrap();
    let wrong_hls = HlsInputKeys::new(vec![(
        KEY_URI.into(),
        Aes128Key::from_hex(&hex::encode(wrong)).unwrap(),
    )])
    .unwrap();
    let candidate = output.join("master.m3u8");
    all_checks(&verify_with_result_keys(&source, &candidate, &empty, &correct_hls).unwrap());
    let result = verify_with_result_keys(&source, &candidate, &empty, &wrong_hls);
    println!(
        "DRMR_AUDIO_CBC_VALID_PADDING_CONTROL {}",
        serde_json::json!({
            "stage": "aes-output-first-init",
            "resource_count": resources.len(),
            "selected_attempt": attempt,
            "wrong_init_padding": resources[0].padding(&wrong),
            "wrong_init_first_box_out_of_bounds": resources[0].invalid_first_box(&wrong),
            "actual_error": result.as_ref().err().map(|error| error.0),
            "expected_error": "fragment-timing-not-supported",
        })
    );
    expected_error(result.unwrap_err(), "fragment-timing-not-supported");
    all_checks(&verify_with_result_keys(&source, &candidate, &empty, &correct_hls).unwrap());
    assert_eq!(
        common::snapshot_tree(source.parent().unwrap()),
        source_before
    );
    assert_eq!(common::snapshot_tree(&output), output_before);
}
