//! Finite audio edits survive a complete progressive MP4 presentation without transcoding.
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn command(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(args)
        .output()
        .unwrap()
}
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4")
}
fn source(path: &Path, video: bool) {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-nostdin", "-i"]).arg(fixture());
    if video {
        cmd.args(["-map", "0:v:0"]);
    }
    cmd.args(["-map", "0:a:0", "-c", "copy"]).arg(path);
    let result = cmd.output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
fn assert_checks(report: &serde_json::Value) {
    assert_eq!(report["passed"], true);
    assert_eq!(report["media_verified"], true);
    assert_eq!(report["license_playback_verified"], false);
    assert_eq!(report["checks"]["stream_metadata"], "pass");
    for check in [
        "packet_payloads_equal",
        "packet_timing_equal",
        "decoded_content_equal",
    ] {
        assert_eq!(report["checks"][check], true, "{report}");
    }
}
#[test]
fn finite_bbb_audio_and_joint_video_convert_and_verify_against_the_whole_original() {
    let root = tempfile::tempdir().unwrap();
    for video in [false, true] {
        let original = root.path().join(format!("source-{video}.mp4"));
        let output = root.path().join(format!("output-{video}.mp4"));
        source(&original, video);
        let before = std::fs::read(&original).unwrap();
        let result = command(&[
            "convert",
            original.to_str().unwrap(),
            "--to",
            "clear",
            "--format",
            "mp4",
            "--output",
            output.to_str().unwrap(),
            "--json",
            "--packager-bin",
            "unused-packager-for-progressive-profile",
        ]);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_checks(&report["verification"]);
        assert_eq!(report["output"]["protection"], "clear");
        assert_eq!(
            report["output"]["tracks"].as_array().unwrap().len(),
            if video { 2 } else { 1 }
        );
        let verify = command(&[
            "verify",
            output.to_str().unwrap(),
            "--source",
            original.to_str().unwrap(),
            "--json",
        ]);
        assert!(
            verify.status.success(),
            "{}",
            String::from_utf8_lossy(&verify.stderr)
        );
        assert_checks(&serde_json::from_slice::<serde_json::Value>(&verify.stdout).unwrap());
        assert_eq!(std::fs::read(&original).unwrap(), before);
        let candidate = std::fs::read(&output).unwrap();
        assert!(
            !drmr_core::mp4::parse_boxes(&candidate)
                .unwrap()
                .iter()
                .any(|b| b.kind() == *b"moof")
        );
    }
}

fn convert(source: &Path, output: &Path) -> Output {
    command(&[
        "convert",
        source.to_str().unwrap(),
        "--to",
        "clear",
        "--output",
        output.to_str().unwrap(),
        "--json",
    ])
}
fn patch_box(source: &Path, output: &Path, kind: [u8; 4], offset: usize, bytes: &[u8]) {
    let mut data = std::fs::read(source).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&data).unwrap();
    let node = drmr_core::mp4::walk(&boxes)
        .into_iter()
        .find(|node| node.kind() == kind)
        .unwrap();
    let at = node.header.offset + node.header.header_size + offset;
    data[at..at + bytes.len()].copy_from_slice(bytes);
    std::fs::write(output, data).unwrap();
}
#[test]
fn unsupported_audio_edits_and_playback_headers_are_refused_without_publication() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.mp4");
    source(&original, false);
    let mut errors = Vec::new();
    for (label, kind, at, value, code) in [
        (
            "multiple",
            *b"elst",
            4,
            2_u32,
            "presentation-window-not-supported",
        ),
        (
            "rate",
            *b"elst",
            16,
            2_u32 << 16,
            "presentation-window-not-supported",
        ),
        (
            "empty",
            *b"elst",
            12,
            u32::MAX,
            "presentation-window-not-supported",
        ),
        (
            "fractional",
            *b"mvhd",
            12,
            48001_u32,
            "progressive-mp4-profile-not-supported",
        ),
        (
            "wrong-handler",
            *b"hdlr",
            8,
            u32::from_be_bytes(*b"vide"),
            "progressive-mp4-profile-not-supported",
        ),
        (
            "movie-rate",
            *b"mvhd",
            20,
            2_u32 << 16,
            "progressive-mp4-profile-not-supported",
        ),
        (
            "movie-gain",
            *b"mvhd",
            24,
            128_u32 << 16,
            "progressive-mp4-profile-not-supported",
        ),
        (
            "track-gain",
            *b"tkhd",
            36,
            128_u32 << 16,
            "progressive-mp4-profile-not-supported",
        ),
        (
            "disabled-track",
            *b"tkhd",
            0,
            0,
            "progressive-mp4-profile-not-supported",
        ),
        (
            "alternate-audio",
            *b"tkhd",
            34,
            (2_u32 << 16) | 256,
            "progressive-mp4-profile-not-supported",
        ),
    ] {
        let input = root.path().join(format!("{label}.mp4"));
        let output = root.path().join(format!("{label}-output.mp4"));
        patch_box(&original, &input, kind, at, &value.to_be_bytes());
        let before = std::fs::read(&input).unwrap();
        let result = convert(&input, &output);
        errors.push((label, code, result));
        assert!(!output.exists());
        assert_eq!(std::fs::read(&input).unwrap(), before);
    }

    for (_, code, result) in errors {
        assert_conversion_failure(&result, code, 1);
    }
}
#[test]
fn a_changed_finite_tail_fails_complete_decode_even_when_all_packet_clocks_match() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.mp4");
    source(&original, false);
    let shorter = root.path().join("shorter.mp4");
    patch_box(&original, &shorter, *b"elst", 8, &479_520_u32.to_be_bytes());
    patch_box(&shorter, &shorter, *b"tkhd", 20, &479_520_u32.to_be_bytes());
    patch_box(&shorter, &shorter, *b"mvhd", 16, &479_520_u32.to_be_bytes());
    assert_eq!(packet_clocks(&original), packet_clocks(&shorter));
    let result = command(&[
        "verify",
        shorter.to_str().unwrap(),
        "--source",
        original.to_str().unwrap(),
        "--json",
    ]);
    assert!(!result.status.success());
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["checks"]["stream_metadata"], "pass");
    assert_eq!(report["checks"]["packet_payloads_equal"], true);
    assert_eq!(report["checks"]["packet_timing_equal"], false);
    assert_eq!(report["checks"]["decoded_content_equal"], false);
}
#[test]
fn nondefault_movie_and_video_clocks_are_preserved_without_rescaling_tolerance() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("clock.mp4");
    let output = root.path().join("output.mp4");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(fixture())
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-movie_timescale",
            "96000",
            "-video_track_timescale",
            "90000",
        ])
        .arg(&original)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let result = convert(&original, &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_checks(&report["verification"]);
}
#[test]
fn b_frames_keep_negative_decode_timestamps_and_the_finite_audio_tail() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("b-frames.mp4");
    let output = root.path().join("output.mp4");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(fixture())
        .args([
            "-map", "0:v:0", "-map", "0:a:0", "-c:v", "libx264", "-threads", "1", "-bf", "2",
            "-c:a", "copy",
        ])
        .arg(&original)
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts,dts",
            "-of",
            "json",
        ])
        .arg(&original)
        .output()
        .unwrap();
    let packets: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    assert!(
        packets["packets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["dts"].as_i64().unwrap() < 0)
    );
    let result = convert(&original, &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_checks(&report["verification"]);
}
#[test]
fn an_empty_offset_edit_is_not_silently_zeroed_or_trimmed() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("offset.mp4");
    let output = root.path().join("output.mp4");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-itsoffset", "0.25", "-i"])
        .arg(fixture())
        .args(["-map", "0:a:0", "-c", "copy"])
        .arg(&original)
        .output()
        .unwrap();
    assert!(status.status.success());
    let before = std::fs::read(&original).unwrap();
    let result = convert(&original, &output);
    assert_conversion_failure(&result, "presentation-window-not-supported", 1);
    assert!(!output.exists());
    assert_eq!(std::fs::read(&original).unwrap(), before);
}

fn converter(ffmpeg: &str) -> drmr_core::conversion::Converter {
    use std::sync::Arc;
    drmr_core::conversion::Converter::new(
        Arc::new(drmr_core::toolrun::ToolRunner::new(
            Arc::new(drmr_core::toolrun::Secrets::new()),
            Arc::new(drmr_core::events::NoSink),
        )),
        "packager",
        ffmpeg,
    )
}
fn assert_conversion_failure(result: &Output, code: &str, exit: i32) {
    assert_eq!(result.status.code(), Some(exit));
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["command"], "convert");
    assert_eq!(report["status"], "failed");
    assert_eq!(report["error"]["code"], code);
    assert!(
        report["error"]["hint"]
            .as_str()
            .is_some_and(|hint| !hint.is_empty())
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains(code));
}

fn protected_mp4(source: &Path, output: &Path, target: &str, keys: Option<&Path>) -> Output {
    let mut args = vec![
        "convert",
        source.to_str().unwrap(),
        "--to",
        target,
        "--format",
        "mp4",
        "--output",
        output.to_str().unwrap(),
        "--json",
    ];
    if let Some(keys) = keys {
        args.extend(["--keys-file", keys.to_str().unwrap()]);
    }
    command(&args)
}

fn encrypted_finite_output(original: &Path, output: &Path, target: &str, keys: &Path) {
    let before = std::fs::read(original).unwrap();
    let result = protected_mp4(original, output, target, Some(keys));
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_checks(&report["verification"]);
    assert_eq!(report["output"]["protection"], target);
    assert_eq!(report["processing"]["route"], "fragmented-mp4");
    assert_eq!(report["output"]["tracks"].as_array().unwrap().len(), 1);
    let candidate = std::fs::read(output).unwrap();
    let verified = command(&[
        "verify",
        output.to_str().unwrap(),
        "--source",
        original.to_str().unwrap(),
        "--keys-file",
        keys.to_str().unwrap(),
        "--json",
    ]);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    assert_checks(&serde_json::from_slice::<serde_json::Value>(&verified.stdout).unwrap());
    let existing = protected_mp4(original, output, target, Some(keys));
    assert_conversion_failure(&existing, "output-exists-or-unsafe", 1);
    assert_eq!(std::fs::read(output).unwrap(), candidate);
    assert_eq!(std::fs::read(original).unwrap(), before);
    for response in [&result, &verified, &existing] {
        for bytes in [&response.stdout, &response.stderr] {
            assert!(!String::from_utf8_lossy(bytes).contains("11111111111111111111111111111111"));
        }
    }
}

#[test]
fn finite_encrypted_audio_converts_and_verifies_without_overwriting_existing_outputs() {
    use drmr_core::{conversion::InputKeys, inspection::TargetProtection};
    use std::io::Write as _;
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("source.mp4");
    source(&original, false);
    let before = std::fs::read(&original).unwrap();
    let keys = root.path().join("private/keys.json");
    drmr_core::native::create_private_dir_all(keys.parent().unwrap()).unwrap();
    let key_bytes = serde_json::to_vec(&serde_json::json!({"schema_version": 1, "output": {
        "kid": "00000000000000000000000000000001", "key": "11111111111111111111111111111111"
    }}))
    .unwrap();
    drmr_core::native::create_private_new(&keys)
        .unwrap()
        .write_all(&key_bytes)
        .unwrap();
    for (target, protection) in [
        ("cenc", TargetProtection::Cenc),
        ("cbcs", TargetProtection::Cbcs),
    ] {
        let output = root.path().join(format!("{target}.mp4"));
        assert_eq!(
            converter("ffmpeg")
                .convert_mp4(&original, &output, protection, &InputKeys::default(), None)
                .unwrap_err()
                .0,
            "output-key-required"
        );
        let missing = protected_mp4(&original, &output, target, None);
        assert_conversion_failure(&missing, "output-key-role-mismatch", 2);
        assert!(!output.exists());
        encrypted_finite_output(&original, &output, target, &keys);
        let protected_before = std::fs::read(&output).unwrap();
        let unkeyed_output = root.path().join(format!("unkeyed-{target}.mp4"));
        let unkeyed = protected_mp4(&output, &unkeyed_output, "clear", None);
        assert_conversion_failure(&unkeyed, "input-key-required", 2);
        assert!(!unkeyed_output.exists());
        assert_eq!(std::fs::read(&output).unwrap(), protected_before);
        assert_eq!(std::fs::read(&original).unwrap(), before);
        assert_eq!(std::fs::read(&keys).unwrap(), key_bytes);
    }
    let output = root.path().join("existing.mp4");
    std::fs::write(&output, b"existing bytes").unwrap();
    assert_eq!(
        converter("ffmpeg")
            .convert_mp4(
                &original,
                &output,
                TargetProtection::Clear,
                &InputKeys::default(),
                None
            )
            .unwrap_err()
            .0,
        "output-exists-or-unsafe"
    );
    assert_eq!(std::fs::read(output).unwrap(), b"existing bytes");
    assert_eq!(std::fs::read(original).unwrap(), before);
}
#[test]
fn only_complete_selectors_from_one_original_presentation_are_admitted() {
    use drmr_core::{
        conversion::{InputKeys, LocalTrack, TrackConversionRequest},
        inspection::{OutputFormat, TargetProtection, TrackKind},
    };
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("source.mp4");
    source(&original, true);
    let other = root.path().join("other.mp4");
    std::fs::copy(&original, &other).unwrap();
    let output = root.path().join("output.mp4");
    let keys = InputKeys::default();
    for tracks in [
        vec![LocalTrack {
            source: original.clone(),
            kind: TrackKind::Video,
            index: 0,
        }],
        vec![
            LocalTrack {
                source: original.clone(),
                kind: TrackKind::Video,
                index: 0,
            },
            LocalTrack {
                source: other,
                kind: TrackKind::Audio,
                index: 1,
            },
        ],
        vec![
            LocalTrack {
                source: original.clone(),
                kind: TrackKind::Video,
                index: 0,
            },
            LocalTrack {
                source: original.clone(),
                kind: TrackKind::Audio,
                index: 0,
            },
        ],
    ] {
        let error = converter("ffmpeg")
            .convert_tracks(TrackConversionRequest {
                tracks: &tracks,
                output: &output,
                format: OutputFormat::Mp4,
                target: TargetProtection::Clear,
                input_keys: &keys,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap_err();
        assert_eq!(error.0, "mp4-requires-one-complete-presentation");
        assert!(!output.exists());
    }
}
#[test]
fn mux_failure_timeout_and_active_cancellation_cannot_publish() {
    use drmr_core::{conversion::InputKeys, inspection::TargetProtection};
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("source.mp4");
    source(&original, false);
    let before = std::fs::read(&original).unwrap();
    let keys = InputKeys::default();
    let failing = drmr_test_support::install(
        &root.path().join("failing"),
        &drmr_test_support::ToolSpec {
            exit_code: 17,
            ..Default::default()
        },
    )
    .unwrap();
    let output = root.path().join("failed.mp4");
    assert_eq!(
        converter(failing.to_str().unwrap())
            .convert_mp4(&original, &output, TargetProtection::Clear, &keys, None)
            .unwrap_err()
            .0,
        "media-tool-failed"
    );
    assert!(!output.exists());
    let slow = drmr_test_support::install(
        &root.path().join("slow"),
        &drmr_test_support::ToolSpec {
            sleep_ms: 30000,
            ..Default::default()
        },
    )
    .unwrap();
    let output = root.path().join("timeout.mp4");
    assert_eq!(
        converter(slow.to_str().unwrap())
            .with_timeout(Duration::from_secs(3))
            .convert_mp4(&original, &output, TargetProtection::Clear, &keys, None)
            .unwrap_err()
            .0,
        "media-tool-timeout"
    );
    assert!(!output.exists());
    let started = root.path().join("started");
    let child = drmr_test_support::install(
        &root.path().join("cancel"),
        &drmr_test_support::ToolSpec {
            sleep_ms: 30000,
            ..Default::default()
        }
        .with("argv_file", started.to_str().unwrap()),
    )
    .unwrap();
    let stop = drmr_core::concurrency::Stop::new();
    let signal = Arc::new(started);
    std::thread::scope(|scope| {
        let observer = scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !signal.is_file() {
                assert!(Instant::now() < deadline, "mux did not start");
                std::thread::yield_now();
            }
            stop.set();
        });
        let output = root.path().join("cancelled.mp4");
        let error = converter(child.to_str().unwrap())
            .with_stop(stop.clone())
            .convert_mp4(&original, &output, TargetProtection::Clear, &keys, None)
            .unwrap_err();
        observer.join().unwrap();
        assert_eq!(error.0, "cancelled");
        assert!(!output.exists());
    });
    assert_eq!(std::fs::read(original).unwrap(), before);
}
#[test]
fn plans_offer_qualified_joint_candidates_and_still_require_runtime_profile_validation() {
    use drmr_core::inspection::{OutputFormat, TargetProtection, inspect, plan};
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("source.mp4");
    source(&original, true);
    for target in [
        TargetProtection::Clear,
        TargetProtection::Cenc,
        TargetProtection::Cbcs,
    ] {
        let report = plan(inspect(&original).unwrap(), target, vec![OutputFormat::Mp4]);
        assert!(report.compatible);
        assert!(report.execution_available);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == "source-profile-validation-required")
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == "progressive-mp4-profile-validation-required")
        );
        let processing = report.processing.unwrap();
        assert_eq!(
            serde_json::to_value(processing.status).unwrap(),
            "metadata-only"
        );
        let routes: Vec<_> = processing
            .candidates
            .iter()
            .map(|candidate| {
                assert!(candidate.revalidation_required);
                assert_eq!(candidate.output_format, OutputFormat::Mp4);
                candidate.route.as_str()
            })
            .collect();
        assert_eq!(
            routes,
            if target == TargetProtection::Clear {
                vec!["progressive-mp4", "fragmented-mp4"]
            } else {
                vec!["fragmented-mp4"]
            }
        );
    }
}

fn packet_clocks(path: &Path) -> serde_json::Value {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "packet=stream_index,pts,dts,duration",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}
fn shorten_video_window(original: &Path, altered: &Path) {
    let mut data = std::fs::read(original).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&data).unwrap();
    let all = drmr_core::mp4::walk(&boxes);
    let movie_header = all.iter().find(|b| b.kind() == *b"mvhd").unwrap();
    let media_header = all.iter().find(|b| b.kind() == *b"mdhd").unwrap();
    let read = |node: &&drmr_core::mp4::Mp4Box, offset| {
        u32::from_be_bytes(
            node.header.payload(&data)[offset..offset + 4]
                .try_into()
                .unwrap(),
        )
    };
    let movie_scale = read(movie_header, 12);
    let video_scale = read(media_header, 12);
    let (mut a, mut b) = (movie_scale, video_scale);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    let step = movie_scale / a;
    for kind in [*b"elst", *b"tkhd"] {
        let node = all.iter().find(|b| b.kind() == kind).unwrap();
        let offset = if kind == *b"elst" { 8 } else { 20 };
        let at = node.header.offset + node.header.header_size + offset;
        let old = u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
        data[at..at + 4].copy_from_slice(&(old - step).to_be_bytes());
    }
    std::fs::write(altered, data).unwrap();
}

#[test]
fn a_video_edit_ending_inside_the_final_frame_cannot_pass_independent_verification() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.mp4");
    source(&original, true);
    let altered = root.path().join("altered.mp4");
    shorten_video_window(&original, &altered);
    assert_eq!(packet_clocks(&original), packet_clocks(&altered));
    let result = command(&[
        "verify",
        altered.to_str().unwrap(),
        "--source",
        original.to_str().unwrap(),
        "--json",
    ]);
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["checks"]["stream_metadata"], "pass");
    assert_eq!(report["checks"]["packet_payloads_equal"], true);
    assert_eq!(report["checks"]["decoded_content_equal"], true);
    assert!(!result.status.success());
    assert_eq!(report["checks"]["packet_timing_equal"], false);
    assert_eq!(report["media_verified"], false);
}
#[test]
fn equivalent_integer_movie_clocks_preserve_the_same_presentation_windows() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.mp4");
    source(&original, true);
    let mut data = std::fs::read(&original).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&data).unwrap();
    for node in drmr_core::mp4::walk(&boxes) {
        let offsets: &[usize] = match &node.kind() {
            b"mvhd" => &[12, 16],
            b"tkhd" => &[20],
            b"elst" => &[8],
            _ => &[],
        };
        for offset in offsets {
            assert_eq!(node.header.payload(&data)[0], 0);
            let at = node.header.offset + node.header.header_size + offset;
            let old = u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
            data[at..at + 4].copy_from_slice(&old.checked_mul(2).unwrap().to_be_bytes());
        }
    }
    let equivalent = root.path().join("equivalent.mp4");
    std::fs::write(&equivalent, data).unwrap();
    assert_eq!(packet_clocks(&original), packet_clocks(&equivalent));
    let result = command(&[
        "verify",
        equivalent.to_str().unwrap(),
        "--source",
        original.to_str().unwrap(),
        "--json",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_checks(&serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap());
}

fn native_cenc(source: &Path, output: &Path, kid: &str, key: &str) {
    let result = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-copyts", "-i"])
        .arg(source)
        .args([
            "-map",
            "0",
            "-c",
            "copy",
            "-avoid_negative_ts",
            "disabled",
            "-use_editlist",
            "1",
            // A fixed public fixture IV makes unkeyed metadata inference fail deterministically.
            "-fflags",
            "+bitexact",
            "-encryption_scheme",
            "cenc-aes-ctr",
            "-encryption_key",
            key,
            "-encryption_kid",
            kid,
        ])
        .arg(output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn independently_verified_progressive_cenc_retains_the_same_finite_windows() {
    use std::io::Write as _;
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.mp4");
    source(&original, true);
    let encrypted = root.path().join("encrypted.mp4");
    let key = "02020202020202020202020202020202";
    let kid = "00000000000000000000000000000001";
    native_cenc(&original, &encrypted, kid, key);
    let keys = root.path().join("private/keys.json");
    drmr_core::native::create_private_dir_all(keys.parent().unwrap()).unwrap();
    let body = serde_json::json!({"schema_version":1,"output":{"kid":kid,"key":key}});
    drmr_core::native::create_private_new(&keys)
        .unwrap()
        .write_all(serde_json::to_string(&body).unwrap().as_bytes())
        .unwrap();
    let result = command(&[
        "verify",
        encrypted.to_str().unwrap(),
        "--source",
        original.to_str().unwrap(),
        "--keys-file",
        keys.to_str().unwrap(),
        "--json",
    ]);
    let checks = serde_json::from_slice::<serde_json::Value>(&result.stdout)
        .ok()
        .map(|report| report["checks"].clone());
    assert!(
        result.status.success(),
        "status={:?}; checks={checks:?}; stderr={}",
        result.status.code(),
        String::from_utf8_lossy(&result.stderr)
    );
    assert_checks(&serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap());
    assert!(!String::from_utf8_lossy(&result.stdout).contains(key));
    assert!(!String::from_utf8_lossy(&result.stderr).contains(key));
}

#[test]
fn nondefault_gain_never_becomes_a_verified_default_gain_presentation() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.mp4");
    source(&original, false);
    let half = root.path().join("half.mp4");
    patch_box(&original, &half, *b"tkhd", 36, &128_u16.to_be_bytes());
    assert_eq!(packet_clocks(&original), packet_clocks(&half));
    let original_bytes = std::fs::read(&original).unwrap();
    let half_bytes = std::fs::read(&half).unwrap();
    for (source, candidate) in [(&half, &original), (&original, &half)] {
        let result = command(&[
            "verify",
            candidate.to_str().unwrap(),
            "--source",
            source.to_str().unwrap(),
            "--json",
        ]);
        assert!(!result.status.success());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("presentation-window-not-supported")
        );
        assert_eq!(result.status.code(), Some(1));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap(),
            serde_json::json!({
                "schema_version": 1,
                "command": "verify",
                "status": "failed",
                "error": {
                    "code": "presentation-window-not-supported",
                    "hint": "Review the error code and source profile before retrying with a new output destination.",
                    "last_activity": "verification"
                }
            })
        );
        assert_eq!(std::fs::read(&original).unwrap(), original_bytes);
        assert_eq!(std::fs::read(&half).unwrap(), half_bytes);
    }
    let output = root.path().join("output.mp4");
    let before = std::fs::read(&half).unwrap();
    let result = convert(&half, &output);
    assert!(!result.status.success());
    assert!(!output.exists());
    assert_eq!(std::fs::read(&half).unwrap(), before);
}

#[test]
fn a_global_display_transform_can_be_preserved_in_the_composed_track_matrix() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.mp4");
    source(&original, true);
    let transformed = root.path().join("movie-transform.mp4");
    patch_box(
        &original,
        &transformed,
        *b"mvhd",
        36,
        &(2_u32 << 16).to_be_bytes(),
    );
    let before = std::fs::read(&transformed).unwrap();
    let output = root.path().join("output.mp4");
    let result = convert(&transformed, &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_checks(&report["verification"]);
    assert_eq!(std::fs::read(&transformed).unwrap(), before);
    let result = command(&[
        "verify",
        output.to_str().unwrap(),
        "--source",
        transformed.to_str().unwrap(),
        "--json",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_checks(&serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap());
    for (path, movie_scale, track_scale) in [(&transformed, 2_u32, 1_u32), (&output, 1, 2)] {
        let bytes = std::fs::read(path).unwrap();
        let boxes = drmr_core::mp4::parse_boxes(&bytes).unwrap();
        let all = drmr_core::mp4::walk(&boxes);
        let movie = all
            .iter()
            .find(|b| b.kind() == *b"mvhd")
            .unwrap()
            .header
            .payload(&bytes);
        let track = all
            .iter()
            .find(|b| b.kind() == *b"tkhd")
            .unwrap()
            .header
            .payload(&bytes);
        assert_eq!(
            u32::from_be_bytes(movie[36..40].try_into().unwrap()),
            movie_scale << 16
        );
        assert_eq!(
            u32::from_be_bytes(track[40..44].try_into().unwrap()),
            track_scale << 16
        );
    }
}
#[test]
fn video_display_extent_is_checked_even_when_all_packets_and_decoded_frames_match() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.mp4");
    source(&original, true);
    for (name, offset, expanded) in [("width", 76, 640_u32), ("height", 80, 360_u32)] {
        let altered = root.path().join(format!("{name}.mp4"));
        patch_box(
            &original,
            &altered,
            *b"tkhd",
            offset,
            &(expanded << 16).to_be_bytes(),
        );
        let before = std::fs::read(&altered).unwrap();
        let output = root.path().join(format!("{name}-output.mp4"));
        let result = convert(&altered, &output);
        assert!(!result.status.success());
        assert!(!output.exists());
        assert_eq!(std::fs::read(&altered).unwrap(), before);
        assert_eq!(packet_clocks(&original), packet_clocks(&altered));
        for (source, result) in [(&original, &altered), (&altered, &original)] {
            let result = command(&[
                "verify",
                result.to_str().unwrap(),
                "--source",
                source.to_str().unwrap(),
                "--json",
            ]);
            assert!(!result.status.success());
            let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(report["checks"]["stream_metadata"], "pass");
            assert_eq!(report["checks"]["packet_payloads_equal"], true);
            assert_eq!(report["checks"]["decoded_content_equal"], true);
            assert_eq!(report["checks"]["packet_timing_equal"], false);
            assert_eq!(report["media_verified"], false);
        }
    }
}
#[test]
fn preserved_track_transform_and_anamorphic_display_size_keep_complete_media_identity() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("anamorphic.mp4");
    let result = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(fixture())
        .args([
            "-map", "0:v:0", "-map", "0:a:0", "-vf", "setsar=2", "-c:v", "libx264", "-threads",
            "1", "-c:a", "copy",
        ])
        .arg(&original)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    patch_box(
        &original,
        &original,
        *b"tkhd",
        40,
        &(2_u32 << 16).to_be_bytes(),
    );
    let before = std::fs::read(&original).unwrap();
    let output = root.path().join("output.mp4");
    let result = convert(&original, &output);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_checks(&report["verification"]);
    assert_eq!(std::fs::read(&original).unwrap(), before);
    let result = command(&[
        "verify",
        output.to_str().unwrap(),
        "--source",
        original.to_str().unwrap(),
        "--json",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_checks(&serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap());
    let bytes = std::fs::read(output).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&bytes).unwrap();
    let all = drmr_core::mp4::walk(&boxes);
    let track = all.iter().find(|b| b.kind() == *b"tkhd").unwrap();
    let payload = track.header.payload(&bytes);
    assert_eq!(
        u32::from_be_bytes(payload[40..44].try_into().unwrap()),
        2 << 16
    );
    assert_eq!(
        u32::from_be_bytes(payload[76..80].try_into().unwrap()),
        640 << 16
    );
}

#[test]
fn encrypted_references_keep_finite_windows_display_sizes_and_metadata_strict() {
    use std::io::Write as _;
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.mp4");
    source(&original, true);
    let encrypted = root.path().join("reference.mp4");
    let input_kid = "00000000000000000000000000000001";
    let input_key = "02020202020202020202020202020202";
    let output_kid = "00000000000000000000000000000002";
    let output_key = "22222222222222222222222222222222";
    native_cenc(&original, &encrypted, input_kid, input_key);
    let protected_output = root.path().join("result.mp4");
    native_cenc(&original, &protected_output, output_kid, output_key);
    let private = root.path().join("private");
    drmr_core::native::create_private_dir_all(&private).unwrap();
    for (name, baseline, protected) in [
        ("clear", &original, false),
        ("cenc", &protected_output, true),
    ] {
        let keys = private.join(format!("{name}.json"));
        let mut config = serde_json::json!({"schema_version":1,
            "input":{"keys":[{"kid":input_kid,"key":input_key}]}});
        if protected {
            config["output"] = serde_json::json!({"kid":output_kid,"key":output_key});
        }
        drmr_core::native::create_private_new(&keys)
            .unwrap()
            .write_all(config.to_string().as_bytes())
            .unwrap();
        let verify = |candidate: &Path| {
            command(&[
                "verify",
                candidate.to_str().unwrap(),
                "--source",
                encrypted.to_str().unwrap(),
                "--keys-file",
                keys.to_str().unwrap(),
                "--json",
            ])
        };
        let positive = verify(baseline);
        assert!(
            positive.status.success(),
            "{}",
            String::from_utf8_lossy(&positive.stdout)
        );
        assert_checks(&serde_json::from_slice::<serde_json::Value>(&positive.stdout).unwrap());
        for change in ["window", "width", "matrix"] {
            let mutant = root.path().join(format!("{name}-{change}.mp4"));
            match change {
                "window" => shorten_video_window(baseline, &mutant),
                "width" => patch_box(
                    baseline,
                    &mutant,
                    *b"tkhd",
                    76,
                    &(640_u32 << 16).to_be_bytes(),
                ),
                _ => patch_box(
                    baseline,
                    &mutant,
                    *b"tkhd",
                    40,
                    &(2_u32 << 16).to_be_bytes(),
                ),
            }
            assert_eq!(packet_clocks(baseline), packet_clocks(&mutant));
            let negative = verify(&mutant);
            assert!(
                !negative.status.success(),
                "{name}-{change}: {}",
                String::from_utf8_lossy(&negative.stdout)
            );
            let report: serde_json::Value = serde_json::from_slice(&negative.stdout).unwrap();
            assert_eq!(report["media_verified"], false);
            assert_eq!(report["checks"]["packet_payloads_equal"], true);
            assert_eq!(report["checks"]["decoded_content_equal"], true);
            if change == "matrix" {
                assert_eq!(report["checks"]["stream_metadata"], "fail");
                assert_eq!(report["checks"]["packet_timing_equal"], true);
            } else {
                assert_eq!(report["checks"]["stream_metadata"], "pass");
                assert_eq!(report["checks"]["packet_timing_equal"], false);
            }
        }
    }
}

fn packet_positions(path: &Path, index: usize) -> Vec<(usize, usize)> {
    let result = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            &index.to_string(),
            "-show_entries",
            "packet=pos,size",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(result.status.success());
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    report["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|packet| {
            (
                packet["pos"].as_str().unwrap().parse().unwrap(),
                packet["size"].as_str().unwrap().parse().unwrap(),
            )
        })
        .collect()
}

fn restore_clear_track(original: &Path, encrypted: &Path, mixed: &Path, index: usize) {
    let clear_bytes = std::fs::read(original).unwrap();
    let mut mixed_bytes = std::fs::read(encrypted).unwrap();
    let original_packets = packet_positions(original, index);
    let encrypted_packets = packet_positions(encrypted, index);
    assert_eq!(original_packets.len(), if index == 0 { 240 } else { 470 });
    assert_eq!(encrypted_packets.len(), original_packets.len());
    // Native CENC preserves packet sizes. Restore only the selected track's exact encoded bytes.
    for ((source, size), (target, encrypted_size)) in
        original_packets.into_iter().zip(encrypted_packets)
    {
        assert_eq!(size, encrypted_size);
        mixed_bytes[target..target + size].copy_from_slice(&clear_bytes[source..source + size]);
    }
    let boxes = drmr_core::mp4::parse_boxes(&mixed_bytes).unwrap();
    let movie = boxes.iter().find(|node| node.kind() == *b"moov").unwrap();
    let track = movie
        .children
        .iter()
        .filter(|node| node.kind() == *b"trak")
        .nth(index)
        .unwrap();
    for node in drmr_core::mp4::walk(std::slice::from_ref(track)) {
        let kind = match &node.kind() {
            b"encv" => *b"avc1",
            b"enca" => *b"mp4a",
            b"sinf" | b"senc" | b"saiz" | b"saio" => *b"free",
            _ => continue,
        };
        let at = node.header.offset + 4;
        mixed_bytes[at..at + 4].copy_from_slice(&kind);
    }
    // Same-size neutral boxes preserve all sample and auxiliary offsets in the other track.
    std::fs::write(mixed, mixed_bytes).unwrap();
}

fn exact_clock_fields(path: &Path) -> Vec<(u64, i64, i64, i64)> {
    packet_clocks(path)["packets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|packet| {
            (
                packet["stream_index"].as_u64().unwrap(),
                packet["pts"].as_i64().unwrap(),
                packet["dts"].as_i64().unwrap(),
                packet["duration"].as_i64().unwrap(),
            )
        })
        .collect()
}

fn mixed_reference_checks(clear_index: usize) {
    use drmr_core::inspection::{Protection, inspect};
    use std::io::Write as _;
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.mp4");
    source(&original, true);
    let original_before = std::fs::read(&original).unwrap();
    let encrypted = root.path().join("encrypted.mp4");
    let kid = "00000000000000000000000000000001";
    let key = "02020202020202020202020202020202";
    native_cenc(&original, &encrypted, kid, key);
    let mixed = root.path().join("mixed.mp4");
    restore_clear_track(&original, &encrypted, &mixed, clear_index);
    let inspection = inspect(&mixed).unwrap();
    assert_eq!(inspection.protection, Protection::Mixed);
    assert_eq!(inspection.tracks[clear_index].protection, Protection::Clear);
    assert_eq!(inspection.tracks[clear_index].key_ids, Vec::<String>::new());
    assert_eq!(
        inspection.tracks[1 - clear_index].protection,
        Protection::Cenc
    );
    assert_eq!(inspection.tracks[1 - clear_index].key_ids, [kid]);
    assert_eq!(exact_clock_fields(&mixed), exact_clock_fields(&original));
    let before = std::fs::read(&mixed).unwrap();
    let private = root.path().join("private");
    drmr_core::native::create_private_dir_all(&private).unwrap();
    let keys = private.join("keys.json");
    let config = serde_json::json!({"schema_version":1,"input":{"keys":[{"kid":kid,"key":key}]}});
    drmr_core::native::create_private_new(&keys)
        .unwrap()
        .write_all(config.to_string().as_bytes())
        .unwrap();
    let verify = |candidate: &Path| {
        command(&[
            "verify",
            candidate.to_str().unwrap(),
            "--source",
            mixed.to_str().unwrap(),
            "--keys-file",
            keys.to_str().unwrap(),
            "--json",
        ])
    };
    // The actual verifier decodes and hashes the clear role unkeyed and the CENC role with its key.
    let positive = verify(&original);
    assert!(
        positive.status.success(),
        "{} {}",
        String::from_utf8_lossy(&positive.stdout),
        String::from_utf8_lossy(&positive.stderr)
    );
    assert_checks(&serde_json::from_slice::<serde_json::Value>(&positive.stdout).unwrap());
    let mut negatives = Vec::new();
    for change in ["window", "width"] {
        let mutant = root.path().join(format!("{change}.mp4"));
        if change == "window" {
            shorten_video_window(&original, &mutant);
        } else {
            patch_box(
                &original,
                &mutant,
                *b"tkhd",
                76,
                &(640_u32 << 16).to_be_bytes(),
            );
        }
        assert_eq!(exact_clock_fields(&mutant), exact_clock_fields(&original));
        let negative = verify(&mutant);
        let report: serde_json::Value = serde_json::from_slice(&negative.stdout).unwrap();
        negatives.push((change, negative.status.success(), report));
    }
    assert_eq!(std::fs::read(&original).unwrap(), original_before);
    assert_eq!(std::fs::read(&mixed).unwrap(), before);
    assert!(
        negatives.iter().all(|(_, accepted, _)| !accepted),
        "clear_index={clear_index}: {negatives:?}"
    );
    for (_, _, report) in negatives {
        assert_eq!(report["media_verified"], false);
        assert_eq!(report["checks"]["stream_metadata"], "pass");
        assert_eq!(report["checks"]["packet_payloads_equal"], true);
        assert_eq!(report["checks"]["decoded_content_equal"], true);
        assert_eq!(report["checks"]["packet_timing_equal"], false);
    }
}

#[test]
fn mixed_clear_video_reference_keeps_finite_windows_and_display_sizes() {
    mixed_reference_checks(0);
}

#[test]
fn mixed_clear_audio_reference_keeps_finite_windows_and_display_sizes() {
    mixed_reference_checks(1);
}
