//! Media parsing, exact packet timing, and subprocess operations on synthetic clips.

use drmr_core::events::{Collector, NoSink};
use drmr_core::media::{
    MediaEngine, MediaError, PacketInfo, Subprocess, codec_from_init, concatenate,
    default_timing_tolerance, packets_from_probe, payload_hashes_from_probe, payloads_from_probe,
    same_timing, segment_timing_from_probe, split_fragmented_mp4,
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

fn engine() -> Subprocess {
    Subprocess::new(Arc::new(ToolRunner::new(
        Arc::new(Secrets::new()),
        Arc::new(NoSink),
    )))
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
    // Errors outside HlsMediaError keep their category (hls_pipeline maps ValueError and
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
    let dir = tempfile::tempdir().unwrap();
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
    let dir = tempfile::tempdir().unwrap();
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
fn stpp_supported(result: &std::process::Output) -> bool {
    let unsupported = b"Error opening output files: Not yet implemented in FFmpeg, patches welcome";
    if result.status.code() == Some(176)
        && result
            .stderr
            .windows(unsupported.len())
            .any(|w| w == unsupported)
    {
        eprintln!("skipped: host FFmpeg lacks TTML-in-MP4 muxing");
        return false;
    }
    assert!(result.status.success(), "TTML fixture encoding unavailable");
    true
}

/// The TTML sample of an ffmpeg STPP mp4 (Python's `<tt(?:\s|>).*?</tt>`) replaced by `xml`,
/// padded to the same size so no box changes.
fn with_document(data: &[u8], xml: &[u8]) -> Vec<u8> {
    let start = data
        .windows(4)
        .position(|w| &w[..3] == b"<tt" && (w[3] == b'>' || w[3].is_ascii_whitespace()))
        .expect("a TTML document");
    let end = start
        + data[start..]
            .windows(5)
            .position(|w| w == b"</tt>")
            .unwrap()
        + 5;
    assert!(xml.len() <= end - start);
    let mut document = xml.to_vec();
    document.resize(end - start, b' ');
    [&data[..start], &document, &data[end..]].concat()
}

#[test]
fn real_imsc_payload_splits_into_one_fragment() {
    if !on_path("ffmpeg") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("subs.srt");
    std::fs::write(&source, "1\n00:00:00,000 --> 00:00:02,000\nHello\n").unwrap();
    let mp4 = dir.path().join("subtitle.mp4");
    let result = ffmpeg(&[
        "-v",
        "error",
        "-y",
        "-i",
        source.to_str().unwrap(),
        "-c:s",
        "ttml",
        "-movflags",
        "+frag_keyframe+delay_moov+default_base_moof",
        "-f",
        "mp4",
        mp4.to_str().unwrap(),
    ]);
    if !stpp_supported(&result) {
        return;
    }
    let qualified = with_document(&std::fs::read(&mp4).unwrap(), b"<tt xmlns=\"http://www.w3.org/ns/ttml\" xmlns:ttp=\"http://www.w3.org/ns/ttml#parameter\" ttp:profile=\"http://www.w3.org/ns/ttml/profile/imsc1/text\"><body><div><p begin=\"0s\" end=\"2s\">Hello</p></div></body></tt>");
    std::fs::write(&mp4, &qualified).unwrap();
    let fragments = split_fragmented_mp4(&mp4, &dir.path().join("init.mp4")).unwrap();
    assert_eq!(fragments.len(), 1);
}

#[test]
fn real_imsc_payloads_are_every_packet_and_never_logged() {
    if !on_path("ffmpeg") || !on_path("ffprobe") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("subs.srt");
    std::fs::write(&source, "1\n00:00:00,000 --> 00:00:02,000\nHello\n").unwrap();
    let base = dir.path().join("base.mp4");
    let result = ffmpeg(&[
        "-v",
        "error",
        "-y",
        "-i",
        source.to_str().unwrap(),
        "-c:s",
        "ttml",
        "-movflags",
        "+frag_keyframe+delay_moov+default_base_moof",
        "-f",
        "mp4",
        base.to_str().unwrap(),
    ]);
    if !stpp_supported(&result) {
        return;
    }
    let data = std::fs::read(&base).unwrap();
    let valid: &[u8] = b"<tt xmlns=\"http://www.w3.org/ns/ttml\" xmlns:ttp=\"http://www.w3.org/ns/ttml#parameter\" ttp:profile=\"http://www.w3.org/ns/ttml/profile/imsc1/text\"><body><div><p begin=\"0s\" end=\"2s\">Hello</p></div></body></tt>";
    let invalid: &[u8] = b"<tt:tt xmlns:tt=\"http://www.w3.org/ns/ttml\" xmlns:ttp=\"http://www.w3.org/ns/ttml#parameter\" ttp:profile=\"http://www.w3.org/ns/ttml/profile/imsc1/image\"><tt:body/></tt:tt>";
    std::fs::write(dir.path().join("first.mp4"), with_document(&data, valid)).unwrap();
    std::fs::write(dir.path().join("second.mp4"), with_document(&data, invalid)).unwrap();
    std::fs::write(
        dir.path().join("concat.txt"),
        "file 'first.mp4'\nfile 'second.mp4'\n",
    )
    .unwrap();
    let combined = dir.path().join("combined.mp4");
    let listing = dir.path().join("concat.txt");
    let result = ffmpeg(&[
        "-v",
        "error",
        "-y",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
        listing.to_str().unwrap(),
        "-map",
        "0",
        "-c",
        "copy",
        "-movflags",
        "+frag_keyframe+delay_moov+default_base_moof",
        "-frag_duration",
        "1",
        "-f",
        "mp4",
        combined.to_str().unwrap(),
    ]);
    assert!(result.status.success());
    assert_eq!(engine().packet_payloads(&combined, 0).unwrap().len(), 2);
    let logged = Arc::new(Collector::default());
    let engine = Subprocess::new(Arc::new(ToolRunner::new(
        Arc::new(Secrets::new()),
        logged.clone(),
    )));
    let payloads = engine.packet_payloads(&combined, 0).unwrap();
    assert!(payloads.len() == 2 && payloads[0].windows(5).any(|w| w == b"Hello"));
    let records = logged.records();
    assert_eq!(
        records
            .iter()
            .map(|r| r["event"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["command", "tool-output"]
    );
    assert!(
        records[0]["argv"]
            .as_array()
            .unwrap()
            .contains(&json!("-show_data"))
    );
    let text = serde_json::to_string(&records).unwrap();
    assert!(
        records[1]["stdout"]
            .as_str()
            .unwrap()
            .starts_with("(stdout not logged: ")
    );
    assert!(!text.contains("Hello") && !text.contains(&hex::encode("Hello")));
    let fragments = split_fragmented_mp4(&combined, &dir.path().join("init.mp4")).unwrap();
    assert_ne!(fragments.len(), 0);
}

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
        playlist.to_str().unwrap(),
    ]);
    assert!(
        ffmpeg(&argv).status.success(),
        "synthetic fixture encoding unavailable"
    );
    playlist
}

#[test]
fn probe_packets_keeps_the_partial_final_aac_packet() {
    if !(on_path("ffmpeg") && on_path("ffprobe")) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
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
    let Some(original) = probe_or_skip(engine().probe_packets(&tail, 1), &tail) else {
        return;
    };
    assert_eq!(original.timescale, 48_000);
    // The regression ADTS cannot preserve: a partial final AAC packet, not padded to 1024.
    assert_eq!(*original.durations.last().unwrap(), 512);
}

#[test]
fn normalized_segments_keep_their_timing_shifted_by_two_seconds() {
    if !(on_path("ffmpeg") && on_path("ffprobe")) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    hls_fmp4(dir.path(), "libx264");
    let source = concatenate(
        &[dir.path().join("init.mp4"), dir.path().join("source1.m4s")],
        &dir.path().join("source_1.mp4"),
    )
    .unwrap();
    let engine = engine();
    let shift = ratio(2, 1);
    for (stream, content_type, suffix) in [(0, "video", "ts"), (1, "audio", "mp4")] {
        let Some(before) = probe_or_skip(engine.probe_packets(&source, stream), &source) else {
            return;
        };
        let normalized = engine
            .normalize_segment(
                &source,
                &dir.path().join(format!("normalized_{stream}.{suffix}")),
                stream,
                &shift,
                content_type,
            )
            .unwrap();
        let after = engine.probe_packets(&normalized, 0).unwrap();
        assert!(
            same_timing(&before, &after, &shift, None).unwrap(),
            "{content_type}: {before:?} -> {after:?}"
        );
    }
}

#[test]
fn decoded_digests_compare_content_not_files() {
    if !on_path("ffmpeg") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
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
    let engine = engine();
    let video = engine.decoded_digest(&a, "video", 0).unwrap();
    assert_eq!(video.len(), 64);
    assert_eq!(video, engine.decoded_digest(&copy, "video", 0).unwrap());
    assert_ne!(video, engine.decoded_digest(&b, "video", 0).unwrap());
    let audio = engine.decoded_digest(&a, "audio", 1).unwrap();
    assert_eq!(
        audio,
        engine.decoded_digest(&b, "audio", 1).unwrap(),
        "the same sine"
    );
    assert_eq!(
        engine
            .decoded_digest(&dir.path().join("absent.mp4"), "video", 0)
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
    let dir = tempfile::tempdir().unwrap();
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
