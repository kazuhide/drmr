//! MPEG-TS outputs retain original access units, absolute clocks and complete decode evidence.
#[path = "support/ts_audio_44100.rs"]
mod audio_44100;
#[path = "support/ts_audio_hls.rs"]
mod audio_hls;
#[path = "support/ts_audio_window.rs"]
mod audio_window;
mod common;
#[path = "support/ts_output_lifecycle.rs"]
mod lifecycle;
#[path = "support/ts_output_negatives.rs"]
mod negatives;
#[path = "support/sample_aes_fixture.rs"]
mod sample_aes_fixture;
use drmr_core::{
    conversion::{
        Aes128Key, ConversionRequest, Converter, HlsInputKeys, HlsOutputKey, InputKeys, SegmentIv,
    },
    events::NoSink,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{path::Path, sync::Arc};

fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            Arc::new(NoSink),
        )),
        "packager",
        "ffmpeg",
    )
}
#[test]
fn target_and_associated_iv_mismatches_fail_before_source_acquisition() {
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let key = HlsOutputKey::new(
        "skd://public-fixture",
        Aes128Key::from_hex("101112131415161718191a1b1c1d1e1f").unwrap(),
    )
    .unwrap();
    let with_iv = HlsOutputKey::new_with_iv(
        "skd://public-fixture",
        Aes128Key::from_hex("101112131415161718191a1b1c1d1e1f").unwrap(),
        SegmentIv::from_hex("303132333435363738393a3b3c3d3e3f").unwrap(),
    )
    .unwrap();
    let cases = [
        (
            OutputFormat::HlsTs,
            TargetProtection::Cenc,
            None,
            "ts-output-profile-not-supported",
        ),
        (
            OutputFormat::Hls,
            TargetProtection::HlsSampleAes,
            Some(&with_iv),
            "target-requires-ts-output",
        ),
        (
            OutputFormat::HlsTs,
            TargetProtection::HlsSampleAes,
            Some(&key),
            "hls-sample-output-iv-required",
        ),
        (
            OutputFormat::HlsTs,
            TargetProtection::HlsAes128,
            Some(&with_iv),
            "output-iv-not-used",
        ),
    ];
    for (format, target, output_key, reason) in cases {
        let result = converter().convert(ConversionRequest {
            input: Path::new("missing-fixture.m3u8"),
            output: Path::new("missing-output"),
            format,
            target,
            input_keys: &empty,
            hls_input_keys: &hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: output_key,
        });
        assert_eq!(result.unwrap_err().0, reason);
    }
}
#[test]
fn legacy_wrappers_refuse_ts_targets_or_unused_associated_ivs_before_source_acquisition() {
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let with_iv = HlsOutputKey::new_with_iv(
        KEY_URI,
        Aes128Key::from_hex(INPUT_KEY).unwrap(),
        SegmentIv::from_hex(INPUT_IV).unwrap(),
    )
    .unwrap();
    let missing = Path::new("missing-fixture.m3u8");
    let output = Path::new("missing-output");
    assert_eq!(
        converter()
            .convert_hls_aes128(missing, output, &empty, &with_iv)
            .unwrap_err()
            .0,
        "output-iv-not-used"
    );
    assert_eq!(
        converter()
            .convert_hls_to_hls_aes128(missing, output, &empty, &hls, &with_iv)
            .unwrap_err()
            .0,
        "output-iv-not-used"
    );
    assert_eq!(
        converter()
            .convert_mp4(
                missing,
                output,
                TargetProtection::HlsSampleAes,
                &empty,
                None
            )
            .unwrap_err()
            .0,
        "target-requires-ts-output"
    );
    assert_eq!(
        converter()
            .convert_from_hls(
                missing,
                output,
                TargetProtection::HlsSampleAes,
                &empty,
                &hls,
                None
            )
            .unwrap_err()
            .0,
        "target-requires-ts-output"
    );
    assert_eq!(
        converter()
            .convert_tracks(drmr_core::conversion::TrackConversionRequest {
                tracks: &[],
                output,
                format: OutputFormat::HlsTs,
                target: TargetProtection::Clear,
                input_keys: &empty,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap_err()
            .0,
        "ts-original-evidence-required"
    );
}

const INPUT_KEY: &str = "101112131415161718191a1b1c1d1e1f";
const INPUT_IV: &str = "303132333435363738393a3b3c3d3e3f";
const OUTPUT_KEY: &str = "202122232425262728292a2b2c2d2e2f";
const OUTPUT_IV: &str = "404142434445464748494a4b4c4d4e4f";
const KEY_URI: &str = sample_aes_fixture::KEY_URI;
fn shaka_source(root: &Path, kind: &str, encrypted: bool) -> std::path::PathBuf {
    drmr_core::native::create_media_dir_all(root).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let path = |p: &Path| drmr_core::manifest_support::path::fspath(p).unwrap();
    let mut argv = vec![
        "packager".into(),
        format!(
            "input={},stream={kind},segment_template={},playlist_name=media.m3u8",
            path(&source),
            path(&root.join("segment-$Number$.ts"))
        ),
        "--hls_master_playlist_output".into(),
        path(&root.join("master.m3u8")),
        "--segment_duration".into(),
        "2".into(),
        "--clear_lead".into(),
        "0".into(),
        "--transport_stream_timestamp_offset_ms".into(),
        "0".into(),
        "--mp4_reset_initial_composition_offset_to_zero=false".into(),
    ];
    if encrypted {
        argv.extend([
            "--enable_raw_key_encryption".into(),
            "--keys".into(),
            format!("label=:key_id=00000000000000000000000000000000:key={INPUT_KEY}"),
            "--iv".into(),
            INPUT_IV.into(),
            "--protection_scheme".into(),
            "cbcs".into(),
            "--hls_key_uri".into(),
            KEY_URI.into(),
        ]);
    }
    let runner = ToolRunner::new(
        Arc::new(Secrets::with([INPUT_KEY, INPUT_IV, KEY_URI])),
        Arc::new(NoSink),
    );
    let shown = ["packager".into(), "[fixture arguments redacted]".into()];
    let result = runner
        .run_tool(
            &argv,
            "packager",
            &drmr_core::toolrun::RunOptions {
                timeout: Some(std::time::Duration::from_secs(30)),
                argv_redacted: Some(&shown),
                max_output_chars: 0,
                log_stdout: false,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.returncode, Some(0));
    root.join("media.m3u8")
}

fn audio_sources(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    audio_sources_at_rate(root, None)
}

fn audio_sources_at_rate(
    root: &Path,
    sample_rate: Option<u32>,
) -> (std::path::PathBuf, std::path::PathBuf) {
    use std::io::Write as _;
    let clear = root.join("clear");
    let sample = root.join("sample");
    drmr_core::native::create_media_dir_all(&clear).unwrap();
    drmr_core::native::create_media_dir_all(&sample).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let path = |p: &Path| drmr_core::manifest_support::path::fspath(p).unwrap();
    let mut argv = vec![
        "ffmpeg".into(),
        "-v".into(),
        "error".into(),
        "-nostdin".into(),
        "-threads".into(),
        "1".into(),
        "-i".into(),
        path(&fixture),
        "-map".into(),
        "0:a:0".into(),
    ];
    if let Some(sample_rate) = sample_rate {
        argv.extend([
            "-c:a".into(),
            "aac".into(),
            "-ar".into(),
            sample_rate.to_string(),
        ]);
    } else {
        argv.extend(["-c".into(), "copy".into()]);
    }
    argv.extend([
        "-hls_time".into(),
        "2".into(),
        "-hls_playlist_type".into(),
        "vod".into(),
        "-hls_segment_filename".into(),
        path(&clear.join("segment-%d.ts")),
        path(&clear.join("media.m3u8")),
    ]);
    let runner = ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink));
    let shown = ["ffmpeg".into(), "[fixture arguments redacted]".into()];
    let result = runner
        .run_tool(
            &argv,
            "ffmpeg",
            &drmr_core::toolrun::RunOptions {
                timeout: Some(std::time::Duration::from_secs(30)),
                argv_redacted: Some(&shown),
                max_output_chars: 0,
                log_stdout: false,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.returncode, Some(0));
    let text = std::fs::read_to_string(clear.join("media.m3u8")).unwrap();
    let key: [u8; 16] = hex::decode(INPUT_KEY).unwrap().try_into().unwrap();
    let iv: [u8; 16] = hex::decode(INPUT_IV).unwrap().try_into().unwrap();
    let audio_config = match sample_rate {
        None | Some(48_000) => [0x11, 0x90],
        Some(44_100) => [0x12, 0x10],
        _ => panic!("unsupported test audio configuration"),
    };
    let mut resources = Vec::new();
    for name in text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        assert!(!name.contains(['/', '\\']));
        let destination = sample.join(name);
        sample_aes_fixture::encrypt_segment_with_audio_config(
            &clear.join(name),
            &destination,
            &key,
            &iv,
            audio_config,
        );
        resources.push(destination);
    }
    sample_aes_fixture::normalize_continuity(&resources);
    let encrypted = text.replacen(
        "#EXTM3U\n",
        &format!("#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"{KEY_URI}\",IV=0x{INPUT_IV}\n"),
        1,
    );
    drmr_core::native::create_media_new(&sample.join("media.m3u8"))
        .unwrap()
        .write_all(encrypted.as_bytes())
        .unwrap();
    (clear.join("media.m3u8"), sample.join("media.m3u8"))
}
fn aes_source(clear: &Path, root: &Path) -> std::path::PathBuf {
    use std::io::Write as _;
    drmr_core::native::create_media_dir_all(root).unwrap();
    let key = Aes128Key::from_hex(INPUT_KEY).unwrap();
    let iv = SegmentIv::from_hex(INPUT_IV).unwrap();
    let text = std::fs::read_to_string(clear).unwrap();
    for name in text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        assert!(!name.contains(['/', '\\']));
        drmr_core::conversion::encrypt_segment(
            &clear.parent().unwrap().join(name),
            &root.join(name),
            &key,
            &iv,
        )
        .unwrap();
    }
    let text = text.replacen(
        "#EXTM3U\n",
        &format!("#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"{KEY_URI}\",IV=0x{INPUT_IV}\n"),
        1,
    );
    let output = root.join("media.m3u8");
    drmr_core::native::create_media_new(&output)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    output
}
fn all_checks(report: &drmr_core::conversion::VerificationReport) {
    use drmr_core::conversion::CheckOutcome;
    assert!(report.passed && report.media_verified);
    assert_eq!(report.checks.stream_metadata, CheckOutcome::Pass);
    assert!(report.checks.packet_payloads_equal);
    assert!(report.checks.packet_timing_equal);
    assert!(report.checks.decoded_content_equal);
    assert!(!report.license_playback_verified);
}
fn original_counts(root: &Path, clear: &Path) -> (Option<u64>, Option<u64>) {
    let directory = root.join("count-evidence");
    drmr_core::native::create_media_dir_all(&directory).unwrap();
    let combined = directory.join("original.ts");
    let mut output = drmr_core::native::create_media_new(&combined).unwrap();
    let text = std::fs::read_to_string(clear).unwrap();
    for name in text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let mut input = std::fs::File::open(clear.parent().unwrap().join(name)).unwrap();
        std::io::copy(&mut input, &mut output).unwrap();
    }
    drop(output);
    let runner = ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink));
    let argv = vec![
        "ffprobe".into(),
        "-v".into(),
        "error".into(),
        "-threads".into(),
        "1".into(),
        "-count_packets".into(),
        "-show_entries".into(),
        "stream=codec_type,nb_read_packets".into(),
        "-of".into(),
        "json".into(),
        drmr_core::manifest_support::path::fspath(&combined).unwrap(),
    ];
    let shown = ["ffprobe".into(), "[fixture arguments redacted]".into()];
    let result = runner
        .run_tool(
            &argv,
            "ffprobe",
            &drmr_core::toolrun::RunOptions {
                timeout: Some(std::time::Duration::from_secs(30)),
                argv_redacted: Some(&shown),
                max_stdout_bytes: 65536,
                max_output_chars: 0,
                log_stdout: false,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.returncode, Some(0));
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let mut video = None;
    let mut audio = None;
    for stream in json["streams"].as_array().unwrap() {
        let count = stream["nb_read_packets"].as_str().unwrap().parse().unwrap();
        match stream["codec_type"].as_str().unwrap() {
            "video" => assert!(video.replace(count).is_none()),
            "audio" => assert!(audio.replace(count).is_none()),
            _ => panic!("unexpected fixture stream"),
        }
    }
    (video, audio)
}
struct OutputMaterial {
    target: TargetProtection,
    key: Option<HlsOutputKey>,
    keys: HlsInputKeys,
}
fn output_materials() -> [OutputMaterial; 3] {
    [
        TargetProtection::Clear,
        TargetProtection::HlsAes128,
        TargetProtection::HlsSampleAes,
    ]
    .map(|target| {
        let key = match target {
            TargetProtection::HlsAes128 => {
                Some(HlsOutputKey::new(KEY_URI, Aes128Key::from_hex(OUTPUT_KEY).unwrap()).unwrap())
            }
            TargetProtection::HlsSampleAes => Some(
                HlsOutputKey::new_with_iv(
                    KEY_URI,
                    Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
                    SegmentIv::from_hex(OUTPUT_IV).unwrap(),
                )
                .unwrap(),
            ),
            _ => None,
        };
        let keys = if target == TargetProtection::Clear {
            HlsInputKeys::default()
        } else {
            HlsInputKeys::new_with_ivs(vec![(
                KEY_URI.into(),
                Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
                (target == TargetProtection::HlsSampleAes)
                    .then(|| SegmentIv::from_hex(OUTPUT_IV).unwrap()),
            )])
            .unwrap()
        };
        OutputMaterial { target, key, keys }
    })
}
fn matrix_row(
    layout: &str,
    input_label: &str,
    source: &Path,
    input_keys: &HlsInputKeys,
    clear: &Path,
    root: &Path,
    counts: (Option<u64>, Option<u64>),
) {
    use drmr_core::conversion::{VerificationRequest, VerificationSource};
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
    for material in output_materials() {
        let target = material.target;
        let output = root.join(format!("output-{input_label}-{}", target.as_str()));
        let result = c
            .convert(ConversionRequest {
                input: source,
                output: &output,
                format: OutputFormat::HlsTs,
                target,
                input_keys: &empty,
                hls_input_keys: input_keys,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: material.key.as_ref(),
            })
            .unwrap_or_else(|error| {
                panic!(
                    "{layout}/{input_label}/{} convert: {}",
                    target.as_str(),
                    error.0
                )
            });
        all_checks(&result.verification);
        assert_eq!(
            serde_json::to_value(&result).unwrap()["processing"]["route"],
            "mpeg-ts-hls-ts"
        );
        assert_eq!(
            result.output.tracks.len(),
            if layout == "joint-av" { 2 } else { 1 }
        );
        for (original, keys) in [(source, input_keys), (clear, &empty_hls)] {
            let proof = c
                .verify(VerificationRequest {
                    source: VerificationSource {
                        input: original,
                        input_keys: &empty,
                        hls_keys: keys,
                    },
                    output: VerificationSource {
                        input: &output.join("master.m3u8"),
                        input_keys: &empty,
                        hls_keys: &material.keys,
                    },
                })
                .unwrap_or_else(|error| {
                    panic!(
                        "{layout}/{input_label}/{} verify: {}",
                        target.as_str(),
                        error.0
                    )
                });
            all_checks(&proof);
        }
        assert_eq!(common::snapshot_tree(source.parent().unwrap()), before);
        let shown = format!(
            "{}{}",
            serde_json::to_string(&events.records()).unwrap(),
            serde_json::to_string(&result).unwrap()
        );
        for secret in [INPUT_KEY, INPUT_IV, OUTPUT_KEY, OUTPUT_IV, KEY_URI] {
            assert!(
                !shown.contains(secret),
                "operator material must not enter retained reports/events"
            );
        }
        println!(
            "DRMR_TS_OUTPUT_CELL {}",
            serde_json::json!({"input": input_label, "target": target.as_str(), "layout": layout,
            "stream_metadata": true, "packet_payloads_equal": true, "packet_timing_equal": true, "decoded_content_equal": true,
            "native_bound_video_aus": counts.0, "native_bound_audio_frames": counts.1,
            "source_immutable": true, "license_playback_verified": false})
        );
    }
}
type MatrixRow = fn(&str, &str, &Path, &HlsInputKeys, &Path, &Path, (Option<u64>, Option<u64>));
fn matrix_with(layout: &str, row: MatrixRow) {
    let root = drmr_core::native::private_tempdir().unwrap();
    let (clear, sample) = if layout == "joint-av" {
        let fixture = sample_aes_fixture::source(&root.path().join("input"), INPUT_KEY, INPUT_IV);
        (fixture.clear, fixture.encrypted)
    } else if layout == "audio" {
        audio_sources(root.path())
    } else {
        (
            shaka_source(&root.path().join("clear"), layout, false),
            shaka_source(&root.path().join("sample"), layout, true),
        )
    };
    let counts = original_counts(root.path(), &clear);
    assert_eq!(
        counts,
        match layout {
            "video" => (Some(240), None),
            "audio" => (None, Some(470)),
            _ => (Some(240), Some(470)),
        }
    );
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
            layout,
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
fn video_all_nine_input_output_protection_combinations_preserve_every_access_unit() {
    matrix_with("video", matrix_row);
}
#[test]
fn joint_av_all_nine_input_output_protection_combinations_preserve_original_offset() {
    matrix_with("joint-av", matrix_row);
}
#[test]
fn audio_all_nine_input_output_protection_combinations_preserve_all_pcm_samples() {
    matrix_with("audio", matrix_row);
}

fn dash_row(
    layout: &str,
    input_label: &str,
    source: &Path,
    input_keys: &HlsInputKeys,
    clear: &Path,
    root: &Path,
    counts: (Option<u64>, Option<u64>),
) {
    use drmr_core::conversion::{ContentKey, OutputKey, VerificationRequest, VerificationSource};
    let empty = InputKeys::default();
    let empty_hls = HlsInputKeys::default();
    let c = converter();
    let before = common::snapshot_tree(source.parent().unwrap());
    let output_kid = "909192939495969798999a9b9c9d9e9f";
    for target in [
        TargetProtection::Clear,
        TargetProtection::Cenc,
        TargetProtection::Cbcs,
    ] {
        let output = root.join(format!("dash-{input_label}-{}", target.as_str()));
        let key = (target != TargetProtection::Clear)
            .then(|| OutputKey::from_hex(output_kid, OUTPUT_KEY).unwrap());
        let result = c
            .convert(ConversionRequest {
                input: source,
                output: &output,
                format: OutputFormat::Dash,
                target,
                input_keys: &empty,
                hls_input_keys: input_keys,
                output_key: key.as_ref(),
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap_or_else(|error| {
                panic!(
                    "{layout}/{input_label}/{} DASH: {}",
                    target.as_str(),
                    error.0
                )
            });
        all_checks(&result.verification);
        assert_eq!(
            serde_json::to_value(&result).unwrap()["processing"]["route"],
            "mpeg-ts-dash"
        );
        let result_keys = if key.is_some() {
            InputKeys::new(vec![ContentKey::from_hex(output_kid, OUTPUT_KEY).unwrap()]).unwrap()
        } else {
            InputKeys::default()
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
                        input: &output.join("manifest.mpd"),
                        input_keys: &result_keys,
                        hls_keys: &empty_hls,
                    },
                })
                .unwrap_or_else(|error| {
                    panic!(
                        "{layout}/{input_label}/{} fresh DASH verify: {}",
                        target.as_str(),
                        error.0
                    )
                }),
            );
        }
        assert_eq!(common::snapshot_tree(source.parent().unwrap()), before);
        println!(
            "DRMR_TS_DASH_CELL {}",
            serde_json::json!({
                "layout": layout, "input": input_label, "target": target.as_str(),
                "stream_metadata": true, "packet_payloads_equal": true, "packet_timing_equal": true,
                "decoded_content_equal": true, "source_immutable": true,
                "native_bound_video_aus": counts.0, "native_bound_audio_frames": counts.1,
                "license_playback_verified": false
            })
        );
    }
}
#[test]
fn dash_video_nine_protection_routes_preserve_every_original_access_unit() {
    matrix_with("video", dash_row);
}
#[test]
fn dash_joint_av_nine_protection_routes_preserve_original_offset_and_audio_tail() {
    matrix_with("joint-av", dash_row);
}
#[test]
fn dash_audio_nine_protection_routes_preserve_all_original_pcm_samples() {
    matrix_with("audio", dash_row);
}

fn clear_dash_request<'a>(
    input: &'a Path,
    output: &'a Path,
    empty: &'a InputKeys,
    hls: &'a HlsInputKeys,
) -> ConversionRequest<'a> {
    ConversionRequest {
        input,
        output,
        format: OutputFormat::Dash,
        target: TargetProtection::Clear,
        input_keys: empty,
        hls_input_keys: hls,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    }
}

fn dash_manifest_mutations(original: &str) -> Vec<String> {
    let doc = roxmltree::Document::parse(original).unwrap();
    let duration = doc
        .root_element()
        .attribute("mediaPresentationDuration")
        .unwrap();
    let ns = duration
        .strip_prefix("PT")
        .unwrap()
        .strip_suffix('S')
        .unwrap()
        .replace('.', "")
        .parse::<u64>()
        .unwrap();
    let template = doc
        .descendants()
        .find(|n| n.has_tag_name(("urn:mpeg:dash:schema:mpd:2011", "SegmentTemplate")))
        .unwrap();
    let offset = template.attribute("presentationTimeOffset").unwrap_or("0");
    let first = doc
        .descendants()
        .find(|n| n.has_tag_name(("urn:mpeg:dash:schema:mpd:2011", "S")))
        .unwrap()
        .attribute("t")
        .unwrap();
    let mut mutations = vec![
        original.replace(
            duration,
            &format!(
                "PT{}.{:09}S",
                (ns - 1) / 1_000_000_000,
                (ns - 1) % 1_000_000_000
            ),
        ),
        original.replace(
            duration,
            &format!(
                "PT{}.{:09}S",
                (ns + 1) / 1_000_000_000,
                (ns + 1) % 1_000_000_000
            ),
        ),
        if original.contains("presentationTimeOffset=") {
            original.replace(
                &format!("presentationTimeOffset=\"{offset}\""),
                &format!(
                    "presentationTimeOffset=\"{}\"",
                    offset.parse::<u64>().unwrap() + 1
                ),
            )
        } else {
            original.replacen(
                "<SegmentTemplate ",
                "<SegmentTemplate presentationTimeOffset=\"1\" ",
                1,
            )
        },
        original.replacen(
            &format!("t=\"{first}\""),
            &format!("t=\"{}\"", first.parse::<u64>().unwrap() + 1),
            1,
        ),
    ];

    mutations.extend(clipped_or_unbounded_manifest(original, &doc));
    mutations
}

fn clipped_or_unbounded_manifest(original: &str, doc: &roxmltree::Document<'_>) -> Vec<String> {
    let ns_uri = "urn:mpeg:dash:schema:mpd:2011";
    let templates: Vec<_> = doc
        .descendants()
        .filter(|n| n.has_tag_name((ns_uri, "SegmentTemplate")))
        .collect();
    let mut first_tick = u64::MAX;
    let mut last_tick = 0;
    for template in &templates {
        assert_eq!(template.attribute("timescale"), Some("90000"));
        let mut end = 0;
        for sample in template
            .descendants()
            .filter(|n| n.has_tag_name((ns_uri, "S")))
        {
            let begin = sample.attribute("t").map_or(end, |v| v.parse().unwrap());
            first_tick = first_tick.min(begin);
            let duration: u64 = sample.attribute("d").unwrap().parse().unwrap();
            let repeats = sample
                .attribute("r")
                .map_or(1, |v| v.parse::<u64>().unwrap() + 1);
            end = begin + duration * repeats;
        }
        last_tick = last_tick.max(end);
    }
    let clipped = first_tick + 1;
    let end_ns =
        u64::try_from((u128::from(last_tick - clipped) * 1_000_000_000).div_ceil(90_000)).unwrap();
    let clipped_duration = format!(
        "PT{}.{:09}S",
        end_ns / 1_000_000_000,
        end_ns % 1_000_000_000
    );
    let mut clipped_edits = Vec::new();
    for template in &templates {
        if let Some(attr) = template.attribute_node("presentationTimeOffset") {
            clipped_edits.push((attr.range_value(), clipped.to_string()));
        } else {
            let at = template.range().start + "<SegmentTemplate".len();
            clipped_edits.push((at..at, format!(" presentationTimeOffset=\"{clipped}\"")));
        }
    }
    let mut missing = original.to_owned();
    let mut removed = Vec::new();
    for node in [
        doc.root_element(),
        doc.descendants()
            .find(|n| n.has_tag_name((ns_uri, "Period")))
            .unwrap(),
    ] {
        for name in ["mediaPresentationDuration", "duration"] {
            if let Some(attr) = node.attribute_node(name) {
                clipped_edits.push((attr.range_value(), clipped_duration.clone()));
                removed.push(attr.range());
            }
        }
    }
    clipped_edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut clipped_text = original.to_owned();
    for (range, value) in clipped_edits {
        clipped_text.replace_range(range, &value);
    }

    removed.sort_by_key(|range| std::cmp::Reverse(range.start));
    for range in removed {
        missing.replace_range(range, "");
    }

    let mut mutations = vec![clipped_text, missing];
    if first_tick > 0 {
        let origin = first_tick - 1;
        let end_ns =
            u64::try_from((u128::from(last_tick - origin) * 1_000_000_000).div_ceil(90_000))
                .unwrap();
        let duration = format!(
            "PT{}.{:09}S",
            end_ns / 1_000_000_000,
            end_ns % 1_000_000_000
        );
        let old_duration = doc
            .root_element()
            .attribute("mediaPresentationDuration")
            .unwrap();
        let earlier = original.replace(old_duration, &duration).replace(
            &format!("presentationTimeOffset=\"{first_tick}\""),
            &format!("presentationTimeOffset=\"{origin}\""),
        );
        assert_ne!(earlier, original);
        mutations.push(earlier);
    }
    mutations
}

#[test]
fn dash_corrupted_period_origin_or_segment_clock_is_refused_without_publication() {
    for layout in ["video", "joint-av", "audio"] {
        corrupted_dash_manifest(layout);
    }
}
fn corrupted_dash_manifest(layout: &str) {
    use drmr_core::conversion::{VerificationRequest, VerificationSource};
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = match layout {
        "joint-av" => {
            sample_aes_fixture::source(&root.path().join("input"), INPUT_KEY, INPUT_IV).clear
        }
        "video" => shaka_source(&root.path().join("input"), "video", false),
        _ => audio_sources(root.path()).0,
    };
    let output = root.path().join("dash");
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let c = converter();
    all_checks(
        &c.convert(clear_dash_request(&input, &output, &empty, &hls))
            .unwrap()
            .verification,
    );
    let manifest = output.join("manifest.mpd");
    let original = std::fs::read_to_string(&manifest).unwrap();
    let mutations = dash_manifest_mutations(&original);
    for (i, invalid) in mutations.into_iter().enumerate() {
        assert_ne!(
            invalid, original,
            "{layout} mutation {i} must change the manifest"
        );
        std::fs::write(&manifest, invalid).unwrap();
        assert!(
            c.verify(VerificationRequest {
                source: VerificationSource {
                    input: &input,
                    input_keys: &empty,
                    hls_keys: &hls
                },
                output: VerificationSource {
                    input: &manifest,
                    input_keys: &empty,
                    hls_keys: &hls
                },
            })
            .is_err(),
            "{layout} mutation {i} must not pass fresh verification"
        );
        let unpublished = root.path().join(format!("refused-{i}"));
        assert!(
            c.convert(clear_dash_request(&manifest, &unpublished, &empty, &hls))
                .is_err()
        );
        assert!(!unpublished.exists());
    }
    std::fs::write(&manifest, original).unwrap();
}
