//! Frozen source records, confined resource verification, and digest consistency.
// Keep related operations together so the test exposes the
// correspondence.
#![allow(clippy::too_many_lines)]

mod support;

use drmr_core::canonical::Dumps;
use drmr_core::events::{Collector, Sink};
use drmr_core::snapshot::{analysis_id, snapshot_digest};
use drmr_core::source::{
    ByteRange, Failure, FetchPolicy, FfprobeProber, Prober, SourceRequest, read_analysis,
    verify_source_snapshot,
};
use drmr_core::toolrun::{Secrets, ToolRunner};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use support::{Analysis, Workspace, media, token};

fn record(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// Write `data` as the snapshot with its digest and id recomputed (an intact snapshot written
/// by another tool, not byte tampering).
fn rewrite(path: &Path, mut data: Value) {
    let map = data.as_object_mut().unwrap();
    map.shift_remove("snapshot_digest");
    map.shift_remove("analysis_id");
    let digest = snapshot_digest(&data);
    data["snapshot_digest"] = json!(digest);
    data["analysis_id"] = json!(analysis_id(&digest));
    std::fs::write(path, Dumps::SORTED.dumps(&data)).unwrap();
}

fn analyzed() -> (Workspace, drmr_core::source::SourceAnalysis) {
    let ws = Workspace::new();
    media(&ws.source, None);
    let result = ws.analyze().unwrap();
    (ws, result)
}

fn analyzed_byte_ranges() -> (Workspace, drmr_core::source::SourceAnalysis) {
    let ws = Workspace::new();
    std::fs::write(
        ws.source.join("all.mp4"),
        b"\x00\x00\x00\x08ftyp\x00\x00\x00\x08moof\x00\x00\x00\x08moof",
    )
    .unwrap();
    std::fs::write(
        ws.source.join("master.m3u8"),
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n\
         #EXT-X-MAP:URI=\"all.mp4\",BYTERANGE=\"8@0\"\n\
         #EXT-X-BYTERANGE:8@8\n#EXTINF:1,\nall.mp4\n\
         #EXT-X-BYTERANGE:8\n#EXTINF:1,\nall.mp4\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    let result = ws.analyze().unwrap();
    (ws, result)
}

#[test]
fn byte_range_snapshots_round_trip_init_and_explicit_or_implicit_media_ranges() {
    let (_ws, result) = analyzed_byte_ranges();
    verify_source_snapshot(&result).unwrap();
    let restored = read_analysis(&result.snapshot_path).unwrap();
    verify_source_snapshot(&restored).unwrap();
    assert_eq!(restored.tracks, result.tracks);
    let track = &restored.tracks[0];
    assert_eq!(
        track
            .segments
            .iter()
            .map(|s| s.byte_range)
            .collect::<Vec<_>>(),
        [
            Some(ByteRange {
                offset: 8,
                length: 8
            }),
            Some(ByteRange {
                offset: 16,
                length: 8
            }),
        ]
    );
    assert_eq!(
        std::fs::read(track.init_path.as_ref().unwrap()).unwrap(),
        b"\x00\x00\x00\x08ftyp"
    );
    for segment in &track.segments {
        assert_eq!(
            std::fs::read(&segment.path).unwrap(),
            b"\x00\x00\x00\x08moof"
        );
    }
    let data = record(&result.snapshot_path);
    assert_eq!(data["resources"][1]["kind"], "init");
    assert_eq!(data["resources"][1]["byte_range"], json!([0, 8]));
    assert_eq!(data["resources"][2]["byte_range"], json!([8, 8]));
    assert_eq!(data["resources"][3]["byte_range"], json!([16, 8]));
}

#[test]
fn byte_range_record_tampering_still_changes_the_snapshot() {
    for target in ["segment", "init-resource"] {
        let (_ws, result) = analyzed_byte_ranges();
        let mut data = record(&result.snapshot_path);
        if target == "segment" {
            data["tracks"][0]["segments"][0]["byte_range"] = json!([9, 8]);
        } else {
            data["resources"][1]["byte_range"] = json!([1, 8]);
        }
        std::fs::write(&result.snapshot_path, Dumps::SORTED.dumps(&data)).unwrap();
        assert_eq!(
            token(read_analysis(&result.snapshot_path)),
            "snapshot-changed",
            "{target}"
        );
    }
}

#[test]
fn malformed_byte_ranges_remain_invalid_even_with_a_recomputed_digest() {
    for range in [
        json!([]),
        json!([8]),
        json!([8, 8, 8]),
        json!([-1, 8]),
        json!([8, 0]),
        json!([8, -1]),
        json!(["8", 8]),
        json!([8.5, 8]),
        json!([true, 8]),
        json!("8,8"),
        json!({"offset": 8, "length": 8}),
    ] {
        for target in ["segment", "resource"] {
            let (_ws, result) = analyzed_byte_ranges();
            let mut data = record(&result.snapshot_path);
            if target == "segment" {
                data["tracks"][0]["segments"][0]["byte_range"] = range.clone();
            } else {
                data["resources"][1]["byte_range"] = range.clone();
            }
            rewrite(&result.snapshot_path, data);
            assert_eq!(
                token(read_analysis(&result.snapshot_path)),
                "snapshot-invalid",
                "{target}: {range}"
            );
        }
    }
}

#[test]
fn segment_ranges_must_match_the_frozen_resource_after_recomputing_the_digest() {
    for range in [json!([9, 8]), Value::Null] {
        let (_ws, result) = analyzed_byte_ranges();
        let mut data = record(&result.snapshot_path);
        data["tracks"][0]["segments"][0]["byte_range"] = range.clone();
        rewrite(&result.snapshot_path, data);
        assert_eq!(
            token(read_analysis(&result.snapshot_path)),
            "snapshot-changed",
            "{range}"
        );
    }
}

#[test]
fn byte_range_snapshot_verification_still_detects_changed_init_and_media_files() {
    for target in ["init", "segment"] {
        let (_ws, result) = analyzed_byte_ranges();
        let restored = read_analysis(&result.snapshot_path).unwrap();
        let track = &restored.tracks[0];
        let path = if target == "init" {
            track.init_path.as_ref().unwrap()
        } else {
            &track.segments[0].path
        };
        std::fs::write(path, b"changed").unwrap();
        assert_eq!(
            token(verify_source_snapshot(&restored)),
            "snapshot-changed",
            "{target}"
        );
        assert_eq!(
            token(read_analysis(&restored.snapshot_path)),
            "snapshot-changed",
            "{target}"
        );
    }
}

#[test]
fn snapshot_total_duration_roundoff_is_portable_but_segments_stay_exact() {
    for (drift, accepted) in [(1e-12, true), (0.001, false)] {
        let (_ws, result) = analyzed();
        let mut data = record(&result.snapshot_path);
        let total = data["tracks"][0]["duration"].as_f64().unwrap() + drift;
        data["tracks"][0]["duration"] = json!(total);
        rewrite(&result.snapshot_path, data);
        if accepted {
            let restored = read_analysis(&result.snapshot_path).unwrap();
            assert_eq!(restored.tracks[0].segments[0].duration, 6.0);
            verify_source_snapshot(&restored).unwrap();
            std::fs::write(&restored.tracks[0].segments[0].path, b"changed").unwrap();
            assert_eq!(token(verify_source_snapshot(&restored)), "snapshot-changed");
        } else {
            assert_eq!(
                token(read_analysis(&result.snapshot_path)),
                "snapshot-changed"
            );
        }
    }
}

#[test]
fn long_legacy_snapshot_keeps_exact_sequential_total() {
    let (_ws, result) = analyzed();
    let mut data = record(&result.snapshot_path);
    let segment = data["tracks"][0]["segments"][0].clone();
    let count = 10_000;
    let duration = 6.006;
    let segments: Vec<Value> = (0..count)
        .map(|index| {
            let mut row = segment.clone();
            row["index"] = json!(index);
            row["duration"] = json!(duration);
            row
        })
        .collect();
    let mut legacy_total = 0.0;
    for _ in 0..count {
        legacy_total += duration;
    }
    let fsum_total = drmr_core::source::fsum::fsum(std::iter::repeat_n(duration, count)).unwrap();
    assert!((legacy_total - fsum_total).abs() > 1e-9);
    data["tracks"][0]["segments"] = Value::Array(segments);
    data["tracks"][0]["duration"] = json!(legacy_total);
    data["tracks"][0]["segment_count"] = json!(count);
    rewrite(&result.snapshot_path, data);
    let restored = read_analysis(&result.snapshot_path).unwrap();
    assert_eq!(restored.tracks[0].segments.len(), count);
    verify_source_snapshot(&restored).unwrap();
}

#[test]
fn a_total_that_is_not_a_number_is_changed_whatever_it_equals() {
    let (_ws, result) = analyzed();
    let mut data = record(&result.snapshot_path);
    data["tracks"][0]["duration"] = json!(true); // `type(...) not in (float, int)`
    rewrite(&result.snapshot_path, data);
    assert_eq!(
        token(read_analysis(&result.snapshot_path)),
        "snapshot-changed"
    );
}

#[test]
fn the_record_compares_as_python_values_compare() {
    // An integer where the tool wrote a float, False for 0: equal under Python's `==`.
    let (_ws, result) = analyzed();
    let mut data = record(&result.snapshot_path);
    data["tracks"][0]["segments"][0]["duration"] = json!(6);
    data["tracks"][0]["segments"][0]["index"] = json!(false);
    data["tracks"][0]["duration"] = json!(6);
    rewrite(&result.snapshot_path, data);
    let restored = read_analysis(&result.snapshot_path).unwrap();
    assert_eq!(
        (
            restored.tracks[0].segments[0].index,
            restored.tracks[0].segments[0].duration
        ),
        (0, 6.0)
    );
    // A string where the tool wrote a number: int("0") is 0, which is not "0".
    let (_ws, result) = analyzed();
    let mut data = record(&result.snapshot_path);
    data["tracks"][0]["segments"][0]["index"] = json!("0");
    rewrite(&result.snapshot_path, data);
    assert_eq!(
        token(read_analysis(&result.snapshot_path)),
        "snapshot-changed"
    );
    // A key the record lacks or carries extra: the dicts differ.
    for edit in ["drop-range", "extra-key"] {
        let (_ws, result) = analyzed();
        let mut data = record(&result.snapshot_path);
        let segment = data["tracks"][0]["segments"][0].as_object_mut().unwrap();
        if edit == "drop-range" {
            segment.shift_remove("byte_range");
        } else {
            segment.insert("extra".into(), json!(1));
        }
        rewrite(&result.snapshot_path, data);
        assert_eq!(
            token(read_analysis(&result.snapshot_path)),
            "snapshot-changed",
            "{edit}"
        );
    }
}

#[test]
fn the_analysis_id_is_reported_as_recorded_not_derived_from_the_digest() {
    // Python compares the recorded id with the analysis's own (both from the same
    // record), never with the digest; so does the implementation.
    let (_ws, result) = analyzed();
    let mut data = record(&result.snapshot_path);
    data["analysis_id"] = json!("hls-000000000000000000000000");
    std::fs::write(&result.snapshot_path, Dumps::SORTED.dumps(&data)).unwrap();
    let restored = read_analysis(&result.snapshot_path).unwrap();
    assert_eq!(restored.analysis_id, "hls-000000000000000000000000");
    let mut altered = restored.clone();
    altered.analysis_id = result.analysis_id.clone();
    assert_eq!(
        token(verify_source_snapshot(&altered)),
        "snapshot-changed",
        "an analysis whose id differs from the record's"
    );
    data["snapshot_digest"] = json!("0".repeat(64));
    std::fs::write(&result.snapshot_path, Dumps::SORTED.dumps(&data)).unwrap();
    assert_eq!(
        token(read_analysis(&result.snapshot_path)),
        "snapshot-changed"
    );
}

#[test]
fn malformed_records_are_invalid_and_what_python_lets_escape_escapes() {
    let (_ws, result) = analyzed();
    let path = &result.snapshot_path;
    let original = record(path);
    std::fs::write(path, "not json").unwrap();
    assert_eq!(token(read_analysis(path)), "snapshot-invalid");
    std::fs::write(path, "[1, 2]").unwrap();
    // `data.get` on a list: an AttributeError escapes read_analysis
    assert_eq!(
        read_analysis(path).unwrap_err(),
        Failure::Escaped(drmr_core::source::Escaped::new("AttributeError"))
    );
    let mut data = original.clone();
    data["schema_version"] = json!("2");
    std::fs::write(path, Dumps::SORTED.dumps(&data)).unwrap();
    assert_eq!(token(read_analysis(path)), "snapshot-invalid");
    let mut data = original.clone();
    data.as_object_mut().unwrap().shift_remove("tracks");
    std::fs::write(path, Dumps::SORTED.dumps(&data)).unwrap();
    assert_eq!(token(read_analysis(path)), "snapshot-invalid");
    let mut data = original.clone();
    data["tracks"][0]["is_default"] = json!(1);
    rewrite(path, data);
    assert_eq!(token(read_analysis(path)), "invalid-selection-flags");
    let mut data = original.clone();
    data["tracks"][0]["segments"][0]["path"] = json!("../outside.ts");
    rewrite(path, data);
    assert_eq!(token(read_analysis(path)), "snapshot-invalid");
    let mut data = original.clone();
    data["tracks"] = json!({});
    data["resources"] = json!([]);
    rewrite(path, data);
    assert!(
        read_analysis(path).unwrap().tracks.is_empty(),
        "an empty mapping iterates as no tracks, as in Python"
    );
}

#[cfg(unix)]
#[test]
fn a_frozen_file_replaced_by_a_link_out_of_the_snapshot_is_invalid() {
    let (ws, result) = analyzed();
    let segment = result.tracks[0].segments[0].path.clone();
    let outside = ws.path().join("outside.ts");
    std::fs::copy(&segment, &outside).unwrap();
    std::fs::remove_file(&segment).unwrap();
    std::os::unix::fs::symlink(&outside, &segment).unwrap();
    assert_eq!(
        token(read_analysis(&result.snapshot_path)),
        "snapshot-invalid"
    );
}

#[test]
fn recorded_paths_join_as_pathlib_joins_them() {
    // `directory / relative` drops `.` components and a trailing `/`: a resource row naming
    // `./r000000.m3u8` or `r000000.m3u8/` verifies (only its bytes are compared), while a
    // segment row's path is compared with the one the analysis would write (`snapshot-changed`).
    for edited in ["./{}", "{}/", "{}/.", ".//{}"] {
        let (_ws, result) = analyzed();
        let mut data = record(&result.snapshot_path);
        let path = data["resources"][0]["path"].as_str().unwrap().to_string();
        data["resources"][0]["path"] = json!(edited.replace("{}", &path));
        rewrite(&result.snapshot_path, data);
        assert!(read_analysis(&result.snapshot_path).is_ok(), "{edited}");
    }
    for edited in ["./{}", "{}/"] {
        let (_ws, result) = analyzed();
        let mut data = record(&result.snapshot_path);
        let path = data["tracks"][0]["segments"][0]["path"]
            .as_str()
            .unwrap()
            .to_string();
        data["tracks"][0]["segments"][0]["path"] = json!(edited.replace("{}", &path));
        rewrite(&result.snapshot_path, data);
        assert_eq!(
            token(read_analysis(&result.snapshot_path)),
            "snapshot-changed",
            "{edited}"
        );
    }
}

#[test]
fn a_snapshot_path_that_is_a_directory_or_missing_is_invalid() {
    let (_ws, result) = analyzed();
    assert_eq!(
        token(read_analysis(result.snapshot_path.parent().unwrap())),
        "snapshot-invalid"
    );
    assert_eq!(
        token(read_analysis(
            &result.snapshot_path.with_file_name("missing.json")
        )),
        "snapshot-invalid"
    );
}

fn on_path(tool: &str) -> bool {
    std::process::Command::new(tool)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
fn stand_in(directory: &Path, name: &str) -> std::path::PathBuf {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ffprobe-stand-in.sh");
    let script = directory.join(name);
    // No executable is opened for writing while concurrent tests spawn children.
    std::os::unix::fs::symlink(fixture, &script).unwrap();
    script
}

#[cfg(windows)]
fn stand_in(directory: &Path, name: &str) -> std::path::PathBuf {
    use drmr_test_support::{Mode, ToolSpec};
    let mut spec = ToolSpec::default();
    spec.stdout = match name {
        "ffprobe" => br#"{"streams":[{"index":0,"codec_type":"video","codec_name":"h264"}],"packets":[{"flags":"K_"}]}"#.to_vec(),
        "exit-error" => { spec.exit_code = 1; Vec::new() }
        "invalid-json" => b"not json".to_vec(),
        "empty-streams" => br#"{"streams":[]}"#.to_vec(),
        "he-aac" => br#"{"streams":[{"codec_type":"audio","codec_name":"aac","profile":"HE-AAC"}]}"#.to_vec(),
        "missing-keyframe" => br#"{"streams":[{"codec_type":"video","codec_name":"h264"}],"packets":[{"flags":"__"}]}"#.to_vec(),
        "packet-error" => {
            // This typed probe supports the streams pass and rejects the packets
            // request with a nonzero status, preserving the actual failure route.
            std::fs::write(directory.join("probe.json"), br#"{"streams":[{"codec_type":"video","codec_name":"h264"}]}"#).unwrap();
            spec = ToolSpec { mode: Mode::Ffprobe, ..ToolSpec::default() }
                .with("root", directory.to_str().unwrap());
            Vec::new()
        }
        _ => panic!("unknown source-probe fixture"),
    };
    drmr_test_support::install(&directory.join(name), &spec).unwrap()
}

#[test]
fn probe_limits_both_ffprobe_decoders_before_input() {
    // ffprobe runs through the tool runner; a stand-in prints what ffprobe prints.
    let tmp = tempfile::tempdir().unwrap();
    let segment = tmp.path().join("segment.ts");
    std::fs::write(&segment, support::ts_packet("", b'x')).unwrap();
    let collector = Arc::new(Collector::default());
    let runner = ToolRunner::new(Arc::new(Secrets::new()), collector.clone() as Arc<dyn Sink>);
    let script = stand_in(tmp.path(), "ffprobe");
    let prober = FfprobeProber::with_program(&runner, &script.to_string_lossy());
    assert_eq!(
        prober.probe(&segment, None).unwrap()[0]["codec_name"],
        "h264"
    );
    let commands: Vec<Vec<String>> = collector
        .records()
        .into_iter()
        .filter(|record| record["event"] == "command")
        .map(|record| {
            record["argv"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_str().unwrap().to_string())
                .collect()
        })
        .collect();
    assert_eq!(commands.len(), 2);
    for command in &commands {
        assert_eq!(command.last().unwrap(), &*segment.to_string_lossy());
        let offset = command
            .iter()
            .position(|token| token == "-threads")
            .unwrap();
        assert!(command[offset + 1] == "1" && offset < command.len() - 2);
    }
    let outputs: Vec<_> = collector
        .records()
        .into_iter()
        .filter(|r| r["event"] == "tool-output")
        .collect();
    assert_eq!(outputs.len(), 2);
    assert!(
        outputs
            .iter()
            .all(|r| r["tool"] == "ffprobe" && r["returncode"] == 0)
    );
}

#[test]
fn the_ffprobe_argv_is_the_python_one() {
    let runner = ToolRunner::new(
        Arc::new(Secrets::new()),
        Arc::new(drmr_core::events::NoSink),
    );
    let prober = FfprobeProber::new(&runner);
    let path = Path::new("/out/a/clear/source/r000001.ts");
    assert_eq!(
        prober.streams_argv(path),
        [
            "ffprobe",
            "-v",
            "error",
            "-threads",
            "1",
            "-show_streams",
            "-show_entries",
            "stream=index,codec_type,codec_name,profile,width,height",
            "-of",
            "json",
            "/out/a/clear/source/r000001.ts"
        ]
    );
    assert_eq!(
        prober.packets_argv(path),
        [
            "ffprobe",
            "-v",
            "error",
            "-threads",
            "1",
            "-select_streams",
            "v:0",
            "-read_intervals",
            "%+#1",
            "-show_packets",
            "-show_entries",
            "packet=flags",
            "-of",
            "json",
            "/out/a/clear/source/r000001.ts"
        ]
    );
}

#[test]
fn a_probe_that_fails_or_finds_no_key_frame_is_classified() {
    let tmp = tempfile::tempdir().unwrap();
    let segment = tmp.path().join("segment.ts");
    std::fs::write(&segment, support::ts_packet("", b'x')).unwrap();
    let diagnostics = Arc::new(Collector::default());
    let runner = ToolRunner::new(Arc::new(Secrets::new()), diagnostics.clone());
    let cases = [
        ("exit-error", "media-probe-failed"),
        ("invalid-json", "media-probe-failed"),
        ("empty-streams", "media-probe-failed"),
        ("he-aac", "unsupported-aac-profile"),
        ("missing-keyframe", "non-independent-video"),
        ("packet-error", "non-independent-video"),
    ];
    for (name, expected) in cases {
        let script = stand_in(tmp.path(), name);
        let prober = FfprobeProber::with_program(&runner, &script.to_string_lossy());
        assert_eq!(
            token(prober.probe(&segment, None)),
            expected,
            "case {name}; tool diagnostics: {:?}",
            diagnostics.records()
        );
    }
    // an encrypted sample entry in the init is refused before ffprobe runs; the probe's
    // temporary copy (init + segment) is removed afterwards
    let init = tmp.path().join("init.mp4");
    std::fs::write(&init, b"\x00\x00\x00\x10ftypisom....encv").unwrap();
    let prober = FfprobeProber::with_program(&runner, "/nonexistent/ffprobe");
    assert_eq!(
        token(prober.probe(&segment, Some(&init))),
        "encrypted-source-unsupported"
    );
    std::fs::write(&init, b"\x00\x00\x00\x08ftyp").unwrap();
    assert_eq!(
        token(prober.probe(&segment, Some(&init))),
        "media-probe-failed",
        "a tool that cannot run"
    );
    assert!(!tmp.path().join("segment.probe.mp4").exists());
    // a WebVTT segment is validated whole, without ffprobe
    let vtt = tmp.path().join("text.vtt");
    std::fs::write(&vtt, b"WEBVTT\n\nhello\n").unwrap();
    assert_eq!(prober.probe(&vtt, None).unwrap()[0]["codec_name"], "webvtt");
    std::fs::write(&vtt, b"WEBVTT\n\n\xff\xfe\n").unwrap();
    assert_eq!(
        token(prober.probe(&vtt, None)),
        "unsupported-subtitle-profile"
    );
}

#[test]
fn real_avc_source_probe_and_frozen_roundtrip() {
    if !on_path("ffmpeg") || !on_path("ffprobe") {
        eprintln!("skipped: ffmpeg / ffprobe not on PATH");
        return;
    }
    for (container, expected) in [("mpegts", "ts"), ("fmp4", "fmp4")] {
        let ws = Workspace::new();
        let status = std::process::Command::new("ffmpeg")
            .current_dir(&ws.source)
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=128x72:r=25:d=2",
                "-c:v",
                "libx264",
                "-g",
                "25",
                "-sc_threshold",
                "0",
                "-an",
                "-f",
                "hls",
                "-hls_time",
                "1",
                "-hls_playlist_type",
                "vod",
                "-hls_segment_type",
                container,
            ])
            .arg("master.m3u8")
            .status()
            .unwrap();
        assert!(status.success());
        let collector = Arc::new(Collector::default());
        let runner = ToolRunner::new(Arc::new(Secrets::new()), collector.clone() as Arc<dyn Sink>);
        let prober = FfprobeProber::new(&runner);
        let result = Analysis::new(&prober)
            .run(
                &SourceRequest::new(ws.source.join("master.m3u8").to_string_lossy()),
                "test-asset",
                &ws.path().join("output"),
                &ws.policy(),
            )
            .unwrap();
        assert_eq!(result.tracks[0].codec, "h264");
        assert_eq!(result.tracks[0].container, expected);
        assert_eq!(result.tracks[0].segments.len(), 2);
        verify_source_snapshot(&read_analysis(&result.snapshot_path).unwrap()).unwrap();
        let commands = collector
            .records()
            .into_iter()
            .filter(|r| r["event"] == "command")
            .count();
        assert_eq!(commands, 4, "two segments, two ffprobe passes each (video)");
        if expected == "fmp4" {
            assert!(result.tracks[0].init_path.is_some());
        }
    }
}

#[test]
fn a_policy_with_a_disk_floor_the_platform_cannot_meet_is_refused_on_the_playlist() {
    let ws = Workspace::new();
    media(&ws.source, None);
    let policy = FetchPolicy {
        min_free_disk_bytes: u64::MAX,
        ..ws.policy()
    };
    let probe: &dyn Prober = &support::default_probe;
    let result = Analysis::new(probe).run(
        &SourceRequest::new(ws.source.join("master.m3u8").to_string_lossy()),
        "floor",
        &ws.path().join("output"),
        &policy,
    );
    assert_eq!(token(result), "disk-budget-exceeded");
}
