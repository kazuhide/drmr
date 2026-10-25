//! Media parsing, exact packet timing, and subprocess operations on synthetic clips.

mod common;
#[path = "support/media_tools.rs"]
mod media_tools;

use drmr_core::events::NoSink;
use drmr_core::media::{
    HlsMediaError, MediaError, PacketInfo, codec_from_init, concatenate, default_timing_tolerance,
    packets_from_probe, payload_hashes_from_probe, payloads_from_probe, same_timing,
    segment_timing_from_probe,
};
use drmr_core::toolrun::{Secrets, ToolRunner};
use num_bigint::BigInt;
use num_rational::BigRational;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn on_path(tool: &str) -> bool {
    let found = std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .any(|dir| drmr_test_support::program_path(&dir.join(tool)).is_file())
    });
    if !found {
        eprintln!("skipped: {tool} not on PATH");
    }
    found
}

/// The major version of the `ffmpeg` on `PATH` (`ffmpeg version 6.1.1-3ubuntu5 …`,
/// `ffmpeg version n8.1.2 …`, `ffmpeg version 9.0.2 …`), or `None` when it cannot be read.
fn ffmpeg_major() -> Option<u32> {
    let output = std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .ok()?;
    ffmpeg_major_of(&String::from_utf8_lossy(&output.stdout))
}

fn ffmpeg_major_of(version_text: &str) -> Option<u32> {
    let word = version_text.lines().next()?.split_whitespace().nth(2)?;
    // Upstream tags carry an `n` (`n8.1.2`); a git snapshot (`N-120000-g…`) names no release.
    let digits: String = word
        .strip_prefix('n')
        .unwrap_or(word)
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The partial-final-AAC-packet fixture requires complete packet timestamps.
/// `FFmpeg` 6.1.1 may omit the final packet's timing fields, so this specific unsupported
/// timing outcome is skipped on that version. Other probe failures remain test failures.
fn probe_or_skip<T, E: std::fmt::Debug>(probed: Result<T, E>, path: &Path) -> Option<T> {
    match probed {
        Ok(info) => Some(info),
        Err(error) => {
            let text = format!("{error:?}");
            match ffmpeg_major() {
                Some(major) if major < 7 && text.contains("unsupported-packet-timing") => {
                    eprintln!(
                        "skipped: ffmpeg {major} omits packet timing on this fixture \
                         (Python's probe_packets raises the same)"
                    );
                    None
                }
                _ => panic!("probe_packets on {}: {text}", path.display()),
            }
        }
    }
}

#[test]
fn ffmpeg_major_is_read_from_distribution_and_upstream_version_lines() {
    assert_eq!(
        ffmpeg_major_of("ffmpeg version 6.1.1-3ubuntu5 Copyright (c) 2000-2023"),
        Some(6)
    );
    assert_eq!(
        ffmpeg_major_of("ffmpeg version n8.1.2 Copyright (c) 2000-2025"),
        Some(8)
    );
    assert_eq!(
        ffmpeg_major_of("ffmpeg version 9.0.2 Copyright (c) 2000-2026"),
        Some(9)
    );
    assert_eq!(
        ffmpeg_major_of("ffmpeg version N-120000-gabcdef Copyright"),
        None
    );
    assert_eq!(ffmpeg_major_of(""), None);
}

fn reference_packets(path: &Path, stream: i64) -> Result<PacketInfo, MediaError> {
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink));
    let completed = media_tools::run(
        &runner,
        &[
            "ffprobe",
            "-threads",
            "1",
            "-v",
            "error",
            "-show_streams",
            "-show_packets",
            "-of",
            "json",
            path.to_str().unwrap(),
        ],
        false,
    )?;
    assert_eq!(completed.returncode, Some(0));
    packets_from_probe(&serde_json::from_slice(&completed.stdout).unwrap(), stream)
}

fn reference_digest(path: &Path, stream: i64) -> Result<String, MediaError> {
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink));
    let completed = media_tools::run(
        &runner,
        &[
            "ffmpeg",
            "-v",
            "error",
            "-threads",
            "1",
            "-i",
            path.to_str().unwrap(),
            "-map",
            &format!("0:{stream}"),
            "-threads",
            "1",
            "-f",
            "hash",
            "-hash",
            "sha256",
            "-",
        ],
        false,
    )?;
    if completed.returncode != Some(0) {
        return Err(HlsMediaError::new("media-decode-failed").into());
    }
    Ok(String::from_utf8(completed.stdout)
        .unwrap()
        .trim()
        .strip_prefix("SHA256=")
        .unwrap()
        .to_owned())
}

fn ratio(numerator: i64, denominator: i64) -> BigRational {
    BigRational::new(numerator.into(), denominator.into())
}

fn packets(
    timescale: i64,
    pts: &[i64],
    dts: &[i64],
    durations: &[i64],
    frame_rate: &str,
) -> PacketInfo {
    PacketInfo {
        timescale,
        pts: pts.to_vec(),
        dts: dts.to_vec(),
        durations: durations.to_vec(),
        frame_rate: frame_rate.into(),
    }
}

fn fixture() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/pure.python311.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn token(error: &MediaError) -> &str {
    error
        .hls_token()
        .unwrap_or_else(|| panic!("not an HlsMediaError: {error:?}"))
}

// ---- same_timing / PacketInfo (the fixture) ----

#[test]
fn timing_rejects_partial_aac_padded_to_full_frame() {
    let source = packets(48_000, &[0, 1024], &[0, 1024], &[1024, 512], "0/0");
    let padded = packets(
        48_000,
        &[96_000, 97_024],
        &[96_000, 97_024],
        &[1024, 1024],
        "0/0",
    );
    assert!(!same_timing(&source, &padded, &ratio(2, 1), None).unwrap());
}

#[test]
fn timing_rejects_decode_shift_with_unchanged_presentation() {
    let source = packets(90_000, &[7500, 11_250], &[0, 3750], &[3750, 3750], "24/1");
    let changed = packets(
        90_000,
        &[187_500, 191_250],
        &[181_000, 184_750],
        &[3750, 3750],
        "24/1",
    );
    assert!(!same_timing(&source, &changed, &ratio(2, 1), None).unwrap());
}

#[test]
fn same_timing_agrees_with_python_on_every_recorded_case() {
    let fixture = fixture();
    let cases = fixture["same_timing"].as_array().unwrap();
    let as_info = |side: &Value| {
        let ints = |v: &Value| {
            v.as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_i64().unwrap())
                .collect::<Vec<_>>()
        };
        PacketInfo {
            timescale: side[0].as_i64().unwrap(),
            pts: ints(&side[1]),
            dts: ints(&side[2]),
            durations: ints(&side[3]),
            frame_rate: "0/0".into(),
        }
    };
    let as_ratio = |pair: &Value| {
        BigRational::new(
            pair[0].as_str().unwrap().parse::<BigInt>().unwrap(),
            pair[1].as_str().unwrap().parse::<BigInt>().unwrap(),
        )
    };
    let (mut same, mut different) = (0, 0);
    for case in cases {
        let tolerance = (!case["tolerance"].is_null()).then(|| as_ratio(&case["tolerance"]));
        let result = same_timing(
            &as_info(&case["source"]),
            &as_info(&case["output"]),
            &as_ratio(&case["shift"]),
            tolerance.as_ref(),
        )
        .unwrap();
        assert_eq!(json!(result), case["same"], "{case}");
        if result { same += 1 } else { different += 1 }
    }
    assert!(
        same > 50 && different > 50,
        "both outcomes covered: {same} / {different}"
    );
}

#[test]
fn same_timing_bounds_are_exact_and_zero_timescales_are_pythons_error() {
    let source = packets(90_000, &[0], &[0], &[3000], "");
    let tick = |pts: i64| packets(90_000, &[pts], &[pts], &[3000], "");
    let shift = ratio(2, 1);
    assert!(
        same_timing(&source, &tick(180_002), &shift, None).unwrap(),
        "two 90 kHz ticks = 1/45000: within"
    );
    assert!(
        !same_timing(&source, &tick(180_003), &shift, None).unwrap(),
        "three ticks: outside"
    );
    assert_eq!(default_timing_tolerance(), ratio(1, 45_000));
    assert!(
        !same_timing(&source, &packets(90_000, &[], &[], &[], ""), &shift, None).unwrap(),
        "count differs"
    );
    let zero = packets(0, &[1], &[1], &[1], "");
    let error = same_timing(&source, &zero, &shift, None).unwrap_err();
    assert_eq!(
        (error.python_class(), error.to_string().as_str()),
        ("ZeroDivisionError", "Fraction(1, 0)")
    );
    assert!(
        same_timing(
            &packets(0, &[], &[], &[], ""),
            &packets(0, &[], &[], &[], ""),
            &shift,
            None
        )
        .unwrap(),
        "nothing compared"
    );
    let info = packets(48_000, &[1024, 0], &[0, 0], &[1024, 512], "");
    assert_eq!(
        (info.start().unwrap(), info.end().unwrap()),
        (ratio(0, 1), ratio(2048, 48_000))
    );
    assert_eq!(
        packets(48_000, &[], &[], &[], "")
            .start()
            .unwrap_err()
            .python_class(),
        "ValueError"
    );
    assert_eq!(
        packets(0, &[5], &[5], &[1], "")
            .end()
            .unwrap_err()
            .python_class(),
        "ZeroDivisionError"
    );
}

// ---- the parse half of probe_packets ----

fn probe_json(streams: Value, packets: Value) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("packets".into(), packets);
    map.insert("streams".into(), streams);
    Value::Object(map)
}

fn video_stream() -> Value {
    json!({"index": 0, "codec_type": "video", "time_base": "1/90000", "avg_frame_rate": "24/1"})
}

#[test]
fn probe_packets_reads_one_streams_timing() {
    let data = probe_json(
        json!([video_stream(), {"index": 1, "time_base": "1/48000"}]),
        json!([{"stream_index": 0, "pts": 7500, "dts": 0, "duration": 3750},
               {"stream_index": 1, "pts": 0, "dts": 0, "duration": 1024},
               {"stream_index": "0", "pts": 11_250, "dts": "3750", "duration": 3750.0}]),
    );
    let info = packets_from_probe(&data, 0).unwrap();
    assert_eq!(
        info,
        packets(90_000, &[7500, 11_250], &[0, 3750], &[3750, 3750], "24/1"),
        "int() of each field"
    );
    let audio = packets_from_probe(&data, 1).unwrap();
    assert_eq!(
        (audio.timescale, audio.frame_rate.as_str()),
        (48_000, ""),
        "no avg_frame_rate: ''"
    );
    let normalized = probe_json(
        json!([{"index": 0, "time_base": "2/180000"}]),
        json!([{"stream_index": 0, "pts": 0, "dts": 0, "duration": 1}]),
    );
    assert_eq!(
        packets_from_probe(&normalized, 0).unwrap().timescale,
        90_000,
        "Fraction normalizes 2/180000"
    );
    let later = probe_json(
        json!([{"index": 0, "time_base": "1/1000"}, {"index": 0, "time_base": "1/25"}]),
        json!([{"stream_index": 0, "pts": 0, "dts": 0, "duration": 1}]),
    );
    assert_eq!(
        packets_from_probe(&later, 0).unwrap().timescale,
        25,
        "a later index replaces an earlier one"
    );
}

#[test]
fn probe_packets_rejects_what_python_rejects() {
    let good = || json!([{"stream_index": 0, "pts": 0, "dts": 0, "duration": 3750}]);
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "stream missing",
            probe_json(json!([video_stream()]), good()),
            "source-stream-missing",
        ),
        (
            "no streams at all",
            probe_json(json!({}), good()),
            "source-stream-missing",
        ),
        (
            "timebase 1001/24000",
            probe_json(json!([{"index": 0, "time_base": "1001/24000"}]), good()),
            "unsupported-timebase",
        ),
        (
            "timebase 0/1",
            probe_json(json!([{"index": 0, "time_base": "0/1"}]), good()),
            "unsupported-timebase",
        ),
        (
            "no packets",
            probe_json(json!([video_stream()]), json!([])),
            "unsupported-packet-timing",
        ),
        (
            "no pts",
            probe_json(
                json!([video_stream()]),
                json!([{"stream_index": 0, "dts": 0, "duration": 1}]),
            ),
            "unsupported-packet-timing",
        ),
        (
            "zero duration",
            probe_json(
                json!([video_stream()]),
                json!([{"stream_index": 0, "pts": 0, "dts": 0, "duration": 0}]),
            ),
            "invalid-packet-duration",
        ),
        (
            "negative duration",
            probe_json(
                json!([video_stream()]),
                json!([{"stream_index": 0, "pts": 0, "dts": 0, "duration": -1}]),
            ),
            "invalid-packet-duration",
        ),
    ];
    for (name, data, expected) in cases {
        let missing_stream = if name == "stream missing" { 3 } else { 0 };
        assert_eq!(
            token(&packets_from_probe(&data, missing_stream).unwrap_err()),
            expected,
            "{name}"
        );
    }
    // Errors outside HlsMediaError keep their category (callers map ValueError and
    // KeyError to track-packaging-failed and lets TypeError / ZeroDivisionError through).
    let classes: Vec<(Value, &str)> = vec![
        (json!({"packets": []}), "KeyError"),
        (json!([]), "TypeError"),
        (
            probe_json(json!([{"time_base": "1/90000"}]), good()),
            "KeyError",
        ),
        (
            probe_json(json!([{"index": "x", "time_base": "1/90000"}]), good()),
            "ValueError",
        ),
        (
            probe_json(json!([{"index": 0, "time_base": "1/0"}]), good()),
            "ZeroDivisionError",
        ),
        (
            probe_json(json!([{"index": 0, "time_base": null}]), good()),
            "TypeError",
        ),
        (
            probe_json(json!([video_stream()]), json!([{"pts": 0}])),
            "KeyError",
        ),
        (
            probe_json(
                json!([video_stream()]),
                json!([{"stream_index": 0, "pts": "x", "dts": 0, "duration": 1}]),
            ),
            "ValueError",
        ),
        (probe_json(json!([video_stream()]), json!(5)), "TypeError"),
    ];
    for (data, class) in classes {
        assert_eq!(
            packets_from_probe(&data, 0).unwrap_err().python_class(),
            class,
            "{data}"
        );
    }
}

// ---- the parse half of packet_payloads (ffprobe -show_data) ----

fn dump(hex_words: &str, ascii: &str) -> String {
    format!("\n00000000: {hex_words:<40}  {ascii}\n")
}

#[test]
fn packet_payloads_reads_the_hex_dump_of_each_packet() {
    let data = json!({"packets": [
        {"stream_index": 0, "size": "5", "data": dump("4865 6c6c 6f", "Hello")},
        {"stream_index": 1, "size": "2", "data": dump("0102", "..")},
        {"stream_index": 0, "size": "18", "data": "\n00000000: 3c74 743e 3c62 6f64 793e 3c2f 626f 6479  <tt><body></body\n00000010: 3e3c                                     ><\n"},
        {"stream_index": "0", "size": "1", "data": dump("ff", ".")},
    ]});
    let payloads = payloads_from_probe(&data, 0).unwrap();
    assert_eq!(
        payloads,
        vec![b"Hello".to_vec(), b"<tt><body></body><".to_vec()],
        "a string index is not 0 (no int())"
    );
    assert_eq!(payloads_from_probe(&data, 1).unwrap(), vec![vec![1, 2]]);
    assert_eq!(
        payloads_from_probe(&json!({"packets": []}), 0).unwrap(),
        Vec::<Vec<u8>>::new()
    );
}

#[test]
fn packet_payloads_rejects_a_malformed_dump_as_text_payload_invalid() {
    let packet = |fields: Value| json!({"packets": [fields]});
    let cases = [
        packet(json!({"stream_index": 0, "size": "4", "data": dump("4865 6c6c 6f", "Hello")})),
        packet(json!({"stream_index": 0, "size": "x", "data": dump("48", "H")})),
        packet(json!({"stream_index": 0, "data": dump("48", "H")})),
        packet(json!({"stream_index": 0, "size": "1"})),
        packet(json!({"stream_index": 0, "size": "1", "data": "\nno offset here\n"})),
        packet(json!({"stream_index": 0, "size": "1", "data": dump("4g", "H")})),
        packet(json!({"size": "1", "data": dump("48", "H")})),
        json!({"no": "packets"}),
        json!({"packets": 3}),
        json!({"packets": ["not a packet"]}),
    ];
    for data in cases {
        assert_eq!(
            token(&payloads_from_probe(&data, 0).unwrap_err()),
            "text-payload-invalid",
            "{data}"
        );
    }
    let not_text = packet(json!({"stream_index": 0, "size": "1", "data": 7}));
    assert_eq!(
        payloads_from_probe(&not_text, 0)
            .unwrap_err()
            .python_class(),
        "AttributeError",
        "Python does not catch it"
    );
}

#[test]
fn payload_hashes_and_segment_timing_parse_as_python_does() {
    let data = json!({"packets": [{"stream_index": 0, "data_hash": "SHA256:aa"}, {"stream_index": 1, "data_hash": "SHA256:bb"}, {"stream_index": 0.0, "data_hash": "SHA256:cc"}]});
    assert_eq!(
        payload_hashes_from_probe(&data, 0).unwrap(),
        vec![json!("SHA256:aa"), json!("SHA256:cc")]
    );
    assert_eq!(
        payload_hashes_from_probe(&json!({"packets": [{"stream_index": 0}]}), 0)
            .unwrap_err()
            .python_class(),
        "KeyError"
    );
    let timing = segment_timing_from_probe(
        &json!({"packets": [{}, {}, {}], "format": {"duration": "2.400000"}}),
    )
    .unwrap();
    assert_eq!((timing.duration_s, timing.packet_count), (2.4, 3));
    let absent = segment_timing_from_probe(&json!({})).unwrap();
    assert!(
        absent.duration_s.is_nan() && absent.packet_count == 0,
        "float('nan'), len([])"
    );
    assert_eq!(
        segment_timing_from_probe(&json!([]))
            .unwrap_err()
            .python_class(),
        "AttributeError"
    );
    assert_eq!(
        segment_timing_from_probe(&json!({"format": null}))
            .unwrap_err()
            .python_class(),
        "AttributeError"
    );
    assert_eq!(
        segment_timing_from_probe(&json!({"format": {"duration": "x"}}))
            .unwrap_err()
            .python_class(),
        "ValueError"
    );
    assert_eq!(
        segment_timing_from_probe(&json!({"packets": null}))
            .unwrap_err()
            .python_class(),
        "TypeError"
    );
}

// ---- codec_from_init, concatenate ----

#[test]
fn codec_from_init_agrees_with_python_on_every_recorded_case() {
    let fixture = fixture();
    let dir = common::private_tempdir().unwrap();
    for case in fixture["codec_from_init"].as_array().unwrap() {
        let name = case[0].as_str().unwrap();
        let path = dir.path().join(format!("{name}.mp4"));
        std::fs::write(&path, hex::decode(case[1].as_str().unwrap()).unwrap()).unwrap();
        let result = codec_from_init(&path, case[2].as_str().unwrap());
        match case[3].as_str().unwrap() {
            "ok" => assert_eq!(result.unwrap(), case[4].as_str().unwrap(), "{name}"),
            class => {
                let error = result.unwrap_err();
                assert_eq!(
                    (error.python_class(), error.to_string()),
                    (class, case[4].as_str().unwrap().to_string()),
                    "{name}"
                );
            }
        }
    }
    assert_eq!(
        codec_from_init(&dir.path().join("absent.mp4"), "aac")
            .unwrap_err()
            .python_class(),
        "OSError"
    );
}

#[test]
fn concatenate_joins_files_in_order_and_creates_the_directory() {
    let dir = common::private_tempdir().unwrap();
    let parts: Vec<PathBuf> = ["a", "b", "c"]
        .iter()
        .map(|name| {
            let path = dir.path().join(name);
            std::fs::write(&path, name.repeat(3)).unwrap();
            path
        })
        .collect();
    let output = dir.path().join("deep/er/joined.mp4");
    assert_eq!(concatenate(&parts, &output).unwrap(), output);
    assert_eq!(std::fs::read_to_string(&output).unwrap(), "aaabbbccc");
    let missing =
        concatenate(&[dir.path().join("absent")], &dir.path().join("out.mp4")).unwrap_err();
    assert_eq!(missing.python_class(), "OSError");
}

// ---- the real tools (the fixture's hls_media steps) ----

fn ffmpeg(args: &[&str]) -> std::process::Output {
    std::process::Command::new("ffmpeg")
        .args(args)
        .output()
        .unwrap()
}

/// `_assert_stpp_fixture_result`: a host ffmpeg without TTML-in-MP4 muxing skips (no packager
/// here, so this is the host-unit tier).
fn hls_fmp4(dir: &Path, codec: &str) -> PathBuf {
    let playlist = dir.join("source.m3u8");
    let mut argv = vec![
        "-v",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=128x72:rate=24",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000",
        "-t",
        "4",
        "-c:v",
        codec,
        "-g",
        "48",
        "-c:a",
        "aac",
        "-b:a",
        "64k",
        "-sc_threshold",
        "0",
    ];
    argv.extend([
        "-f",
        "hls",
        "-hls_time",
        "2",
        "-hls_segment_type",
        "fmp4",
        "-hls_playlist_type",
        "vod",
        "source.m3u8",
    ]);
    assert!(
        std::process::Command::new("ffmpeg")
            .current_dir(dir)
            .args(&argv)
            .output()
            .unwrap()
            .status
            .success(),
        "synthetic fixture encoding unavailable"
    );
    playlist
}

#[test]
fn probe_packets_keeps_the_partial_final_aac_packet() {
    if !(on_path("ffmpeg") && on_path("ffprobe")) {
        return;
    }
    let dir = common::private_tempdir().unwrap();
    hls_fmp4(dir.path(), "libx264");
    let mut segments: Vec<PathBuf> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "m4s"))
        .collect();
    segments.sort();
    let tail = concatenate(
        &[
            dir.path().join("init.mp4"),
            segments.last().unwrap().clone(),
        ],
        &dir.path().join("tail.mp4"),
    )
    .unwrap();
    let Some(original) = probe_or_skip(reference_packets(&tail, 1), &tail) else {
        return;
    };
    assert_eq!(original.timescale, 48_000);
    // The regression ADTS cannot preserve: a partial final AAC packet, not padded to 1024.
    assert_eq!(*original.durations.last().unwrap(), 512);
}

#[test]
fn decoded_digests_compare_content_not_files() {
    if !on_path("ffmpeg") {
        return;
    }
    let dir = common::private_tempdir().unwrap();
    let make = |name: &str, pattern: &str| {
        let path = dir.path().join(name);
        let source = format!("{pattern}=duration=1:size=64x64:rate=5");
        assert!(
            ffmpeg(&[
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                &source,
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "1",
                "-c:v",
                "libx264",
                "-c:a",
                "aac",
                path.to_str().unwrap()
            ])
            .status
            .success()
        );
        path
    };
    let (a, b) = (make("a.mp4", "testsrc"), make("b.mp4", "testsrc2"));
    let copy = dir.path().join("copy.mp4");
    std::fs::copy(&a, &copy).unwrap();
    let video = reference_digest(&a, 0).unwrap();
    assert_eq!(video.len(), 64);
    assert_eq!(video, reference_digest(&copy, 0).unwrap());
    assert_ne!(video, reference_digest(&b, 0).unwrap());
    let audio = reference_digest(&a, 1).unwrap();
    assert_eq!(audio, reference_digest(&b, 1).unwrap(), "the same sine");
    assert_eq!(
        reference_digest(&dir.path().join("absent.mp4"), 0)
            .unwrap_err()
            .hls_token(),
        Some("media-decode-failed")
    );
}

#[test]
fn codec_from_init_reads_a_real_x264_init() {
    if !on_path("ffmpeg") {
        return;
    }
    let dir = common::private_tempdir().unwrap();
    hls_fmp4(dir.path(), "libx264");
    let init = std::fs::read(dir.path().join("init.mp4")).unwrap();
    let at = init.windows(4).position(|w| w == b"avcC").unwrap();
    let expected = format!("avc1.{}", hex::encode(&init[at + 5..at + 8]));
    assert_eq!(
        codec_from_init(&dir.path().join("init.mp4"), "h264").unwrap(),
        expected
    );
    assert!(
        expected.starts_with("avc1.64"),
        "x264's High profile: {expected}"
    );
}
