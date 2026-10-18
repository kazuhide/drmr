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
    for (label, kind, at, value) in [
        ("multiple", *b"elst", 4, 2_u32),
        ("rate", *b"elst", 16, 2_u32 << 16),
        ("empty", *b"elst", 12, u32::MAX),
        ("fractional", *b"mvhd", 12, 48001_u32),
        ("wrong-handler", *b"hdlr", 8, u32::from_be_bytes(*b"vide")),
        ("movie-rate", *b"mvhd", 20, 2_u32 << 16),
        ("movie-gain", *b"mvhd", 24, 128_u32 << 16),
        ("track-gain", *b"tkhd", 36, 128_u32 << 16),
        ("disabled-track", *b"tkhd", 0, 0),
        ("alternate-audio", *b"tkhd", 34, (2_u32 << 16) | 256),
    ] {
        let input = root.path().join(format!("{label}.mp4"));
        let output = root.path().join(format!("{label}-output.mp4"));
        patch_box(&original, &input, kind, at, &value.to_be_bytes());
        let before = std::fs::read(&input).unwrap();
        let result = convert(&input, &output);
        assert!(!result.status.success(), "{label}");
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("progressive-mp4-profile-not-supported"),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!output.exists());
        assert_eq!(std::fs::read(&input).unwrap(), before);
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
    let result = convert(&original, &output);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("progressive-mp4-profile-not-supported")
    );
    assert!(!output.exists());
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
#[test]
fn unsupported_encrypted_output_has_no_clear_fallback_and_existing_outputs_are_untouched() {
    use drmr_core::{
        conversion::{InputKeys, OutputKey},
        inspection::TargetProtection,
    };
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("source.mp4");
    source(&original, false);
    let keys = InputKeys::default();
    let key = OutputKey::from_hex(
        "00000000000000000000000000000001",
        "11111111111111111111111111111111",
    )
    .unwrap();
    for target in [TargetProtection::Cenc, TargetProtection::Cbcs] {
        let output = root.path().join(format!("{}.mp4", target.as_str()));
        let error = converter("ffmpeg")
            .convert_mp4(&original, &output, target, &keys, Some(&key))
            .unwrap_err();
        assert_eq!(error.0, "finite-audio-tail-not-supported");
        assert!(!output.exists());
    }
    let output = root.path().join("existing.mp4");
    std::fs::write(&output, b"existing bytes").unwrap();
    assert_eq!(
        converter("ffmpeg")
            .convert_mp4(&original, &output, TargetProtection::Clear, &keys, None)
            .unwrap_err()
            .0,
        "output-exists-or-unsafe"
    );
    assert_eq!(std::fs::read(output).unwrap(), b"existing bytes");
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
fn plans_offer_only_the_clear_joint_candidate_and_still_require_runtime_profile_validation() {
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
        assert_eq!(
            report.execution_available,
            target == TargetProtection::Clear
        );
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == "source-profile-validation-required")
        );
        if target == TargetProtection::Clear {
            assert!(
                report
                    .findings
                    .iter()
                    .any(|f| f.code == "progressive-mp4-profile-validation-required")
            );
        }
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
#[test]
fn a_video_edit_ending_inside_the_final_frame_cannot_pass_independent_verification() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.mp4");
    source(&original, true);
    let mut data = std::fs::read(&original).unwrap();
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
    let altered = root.path().join("altered.mp4");
    std::fs::write(&altered, data).unwrap();
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

#[test]
fn independently_verified_progressive_cenc_retains_the_same_finite_windows() {
    use std::io::Write as _;
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original.mp4");
    source(&original, true);
    let encrypted = root.path().join("encrypted.mp4");
    let key = "11111111111111111111111111111111";
    let kid = "00000000000000000000000000000001";
    let result = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-copyts", "-i"])
        .arg(&original)
        .args([
            "-map",
            "0",
            "-c",
            "copy",
            "-avoid_negative_ts",
            "disabled",
            "-use_editlist",
            "1",
            "-encryption_scheme",
            "cenc-aes-ctr",
            "-encryption_key",
            key,
            "-encryption_kid",
            kid,
        ])
        .arg(&encrypted)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
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
    assert!(
        result.status.success(),
        "{}",
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
        assert!(result.stdout.is_empty());
    }
    let output = root.path().join("output.mp4");
    let before = std::fs::read(&half).unwrap();
    let result = convert(&half, &output);
    assert!(!result.status.success());
    assert!(!output.exists());
    assert_eq!(std::fs::read(&half).unwrap(), before);
}
