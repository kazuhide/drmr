//! Local clear-HLS admission, policy checks, and injected media probes.
// Keep related operations together so the test exposes the
// correspondence.
#![allow(clippy::too_many_lines)]

mod support;

/// A policy edit of the resource-limit cases.
type Setting = fn(&mut FetchPolicy);

use drmr_core::events::Collector;
use drmr_core::m3u8::parse_attributes;
use drmr_core::source::{
    Escaped, Failure, FetchPolicy, ProbeStream, SourceRequest, read_analysis,
    verify_source_snapshot,
};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use support::{Analysis, FnHooks, Workspace, default_probe, fixed, media, stream, streams, token};

fn events_of(collector: &Collector) -> Vec<Value> {
    collector.records().into_iter().map(Value::Object).collect()
}

fn messages(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["message"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn quoted_attributes() {
    let a = parse_attributes(r#"TYPE=AUDIO,NAME="English, descriptive",URI="audio/a.m3u8?x=1,2""#)
        .unwrap();
    assert_eq!(a.get("TYPE"), Some("AUDIO"));
    assert_eq!(a.get("NAME"), Some("English, descriptive"));
    assert_eq!(a.get("URI"), Some("audio/a.m3u8?x=1,2"));
    assert_eq!(a.len(), 3);
    assert_eq!(
        parse_attributes(r#"NAME="unterminated"#)
            .unwrap_err()
            .token(),
        "invalid-attributes"
    );
}

#[test]
fn analysis_progress_precedes_io_and_probes_every_segment() {
    let ws = Workspace::new();
    let count = 23;
    let body = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n{}#EXT-X-ENDLIST\n",
        "#EXTINF:6,\nsegment.ts\n".repeat(count)
    );
    media(&ws.source, Some(&body));
    let collector = Arc::new(Collector::default());
    let reads = Arc::new(Mutex::new(Vec::new()));
    let probes = Arc::new(AtomicUsize::new(0));
    let first_is_started = |collector: &Collector| {
        collector
            .records()
            .first()
            .and_then(|e| e.get("message").cloned())
            == Some(json!("analysis-started"))
    };
    let hooks = FnHooks {
        read: Some(Box::new({
            let (collector, reads) = (Arc::clone(&collector), Arc::clone(&reads));
            move |fetcher, url, playlist| {
                assert!(first_is_started(&collector));
                reads.lock().unwrap().push(url.to_string());
                fetcher.read(url, playlist, None)
            }
        })),
        fetch: Some(Box::new({
            let (collector, reads) = (Arc::clone(&collector), Arc::clone(&reads));
            move |fetcher, url, destination, range| {
                assert!(first_is_started(&collector));
                reads.lock().unwrap().push(url.to_string());
                fetcher.fetch(url, destination, range)
            }
        })),
    };
    let probe = {
        let (collector, probes) = (Arc::clone(&collector), Arc::clone(&probes));
        move |path: &Path, _init: Option<&Path>| -> Result<Vec<ProbeStream>, Failure> {
            // A segment is inspected as soon as it has landed : its own file exists and
            // the playlist's inspection has been announced.
            assert!(
                collector
                    .records()
                    .iter()
                    .any(|e| e["message"] == "probe-started")
            );
            assert!(path.is_file() && std::fs::metadata(path).unwrap().len() == 188);
            probes.fetch_add(1, Ordering::SeqCst);
            Ok(streams(&[
                json!({"index": 0, "codec_type": "video", "codec_name": "h264"}),
            ]))
        }
    };
    let result = Analysis::new(&probe)
        .hooks(&hooks)
        .log(&*collector)
        .run(
            &SourceRequest::new(ws.source.join("master.m3u8").to_string_lossy()),
            "progress",
            &ws.path().join("output"),
            &ws.policy(),
        )
        .unwrap();
    assert!(result.admitted);
    assert_eq!(probes.load(Ordering::SeqCst), count);
    assert_eq!(reads.lock().unwrap().len(), count + 1);
    let events = events_of(&collector);
    for name in ["segment-downloaded", "segment-probed"] {
        let rows: Vec<&Value> = events.iter().filter(|e| e["message"] == name).collect();
        assert_eq!(
            rows.iter()
                .map(|e| e["index"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            [1, 10, 20, 23]
        );
        assert!(
            rows.iter()
                .all(|e| e["total"] == 23 && e["rendition_id"] == "r000000")
        );
    }
    let order = messages(&events);
    let at = |name: &str| order.iter().position(|m| m == name).unwrap();
    assert!(at("probe-started") < at("playlist-complete"));
    assert_eq!((at("analysis-started"), at("playlist-started")), (0, 1));
    let last = events.last().unwrap();
    assert_eq!(
        (last["status"].as_str(), last["track_count"].as_u64()),
        (Some("pass"), Some(1))
    );
    let mut names: Vec<String> = order.clone();
    names.sort();
    names.dedup();
    assert_eq!(
        names,
        [
            "analysis-complete",
            "analysis-started",
            "playlist-complete",
            "playlist-started",
            "probe-started",
            "segment-downloaded",
            "segment-probed"
        ]
    );
    let allowed = [
        "stage",
        "level",
        "message",
        "rendition_id",
        "index",
        "total",
        "track_count",
        "status",
    ];
    for event in &events {
        assert_eq!(
            (event["stage"].as_str(), event["level"].as_str()),
            (Some("hls-source"), Some("info"))
        );
        assert!(
            event
                .as_object()
                .unwrap()
                .keys()
                .all(|key| allowed.contains(&key.as_str()))
        );
        assert!(
            event.get("ts").is_none(),
            "the clear-HLS records carry no ts"
        );
    }
    let text = serde_json::to_string(&events).unwrap();
    assert!(!text.contains(&*ws.source.to_string_lossy()) && !text.contains("segment.ts"));
}

#[test]
fn analysis_progress_starts_even_when_first_network_request_fails() {
    let ws = Workspace::new();
    let collector = Arc::new(Collector::default());
    let hooks = FnHooks {
        read: Some(Box::new({
            let collector = Arc::clone(&collector);
            move |_, _, _| {
                let events = events_of(&collector);
                assert_eq!(
                    events,
                    vec![
                        json!({"stage": "hls-source", "level": "info", "message": "analysis-started"})
                    ]
                );
                Err(Failure::Source(drmr_core::source::SourceError::new(
                    "source-read-failed",
                )))
            }
        })),
        fetch: None,
    };
    let result = Analysis::new(&default_probe)
        .hooks(&hooks)
        .log(&*collector)
        .run(
            &SourceRequest::new("https://example.test/private.m3u8?token=sentinel"),
            "progress",
            ws.path(),
            &FetchPolicy::default(),
        );
    assert_eq!(token(result), "source-read-failed");
    assert!(
        !serde_json::to_string(&events_of(&collector))
            .unwrap()
            .contains("sentinel")
    );
}

#[test]
fn throttled_progress_does_not_skip_intermediate_codec_admission() {
    let ws = Workspace::new();
    let body = format!(
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n{}#EXT-X-ENDLIST\n",
        "#EXTINF:6,\nsegment.ts\n".repeat(23)
    );
    media(&ws.source, Some(&body));
    let count = AtomicUsize::new(0);
    let probe = |path: &Path, _: Option<&Path>| -> Result<Vec<ProbeStream>, Failure> {
        count.fetch_add(1, Ordering::SeqCst);
        // r000013 is the 13th segment (r000000 is the playlist), outside the checkpoints
        // 1/10/20/23; probes run in landing order, so the call count would not name a segment.
        let codec = if path.file_stem().unwrap() == "r000013" {
            "hevc"
        } else {
            "h264"
        };
        Ok(streams(&[
            json!({"index": 0, "codec_type": "video", "codec_name": codec}),
        ]))
    };
    let collector = Collector::default();
    let policy = FetchPolicy {
        probe_workers: Some(1),
        ..ws.policy()
    };
    let result = Analysis::new(&probe).log(&collector).run(
        &SourceRequest::new(ws.source.join("master.m3u8").to_string_lossy()),
        "progress",
        &ws.path().join("output"),
        &policy,
    );
    assert_eq!(token(result), "changing-codec-configuration");
    assert_eq!(count.load(Ordering::SeqCst), 23);
    assert!(
        !events_of(&collector)
            .iter()
            .any(|e| e.get("status") == Some(&json!("pass")))
    );
    assert!(!ws.path().join("output/progress/clear/source").exists());
}

#[cfg(unix)]
#[test]
fn local_vod_freezes_readonly_and_hides_paths() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let ws = Workspace::new();
    media(&ws.source, None);
    let original = std::fs::metadata(ws.source.join("segment.ts")).unwrap();
    let result = ws.analyze().unwrap();
    assert!(result.admitted && result.tracks[0].id == "v000");
    let segment = &result.tracks[0].segments[0];
    assert_eq!(
        std::fs::read(&segment.path).unwrap(),
        std::fs::read(ws.source.join("segment.ts")).unwrap()
    );
    assert_eq!(
        std::fs::metadata(&segment.path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&result.snapshot_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let after = std::fs::metadata(ws.source.join("segment.ts")).unwrap();
    assert_eq!(
        (after.mtime(), after.mtime_nsec()),
        (original.mtime(), original.mtime_nsec())
    );
    let public = serde_json::to_string(&result.to_public_dict()).unwrap();
    assert!(!public.contains(&*ws.source.to_string_lossy()));
    let restored = read_analysis(&result.snapshot_path).unwrap();
    verify_source_snapshot(&restored).unwrap();
    std::fs::write(&restored.tracks[0].segments[0].path, b"changed").unwrap();
    assert_eq!(token(verify_source_snapshot(&restored)), "snapshot-changed");
}

#[test]
fn master_enumerates_video_audio_subtitle_and_relative_uris() {
    let ws = Workspace::new();
    let root = &ws.source;
    std::fs::write(root.join("master.m3u8"), "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"English, AD\",LANGUAGE=\"en\",CHARACTERISTICS=\"public.accessibility.describes-video\",URI=\"audio.m3u8\"\n#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"Japanese\",LANGUAGE=\"ja\",URI=\"text.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=400000,CODECS=\"avc1.64001e,mp4a.40.2\",RESOLUTION=640x360,AUDIO=\"audio\",SUBTITLES=\"subs\"\nvideo.m3u8\n").unwrap();
    for (name, segment) in [
        ("video", "segment.ts"),
        ("audio", "audio.ts"),
        ("text", "text.vtt"),
    ] {
        std::fs::write(
            root.join(format!("{name}.m3u8")),
            format!("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\n{segment}\n#EXT-X-ENDLIST\n"),
        )
        .unwrap();
    }
    std::fs::write(root.join("segment.ts"), support::ts_packet("", b'x')).unwrap();
    std::fs::write(root.join("audio.ts"), support::ts_packet("", b'a')).unwrap();
    std::fs::write(
        root.join("text.vtt"),
        b"WEBVTT\n\n00:00.000 --> 00:01.000\nhello\n",
    )
    .unwrap();
    let probe = |path: &Path, _: Option<&Path>| -> Result<Vec<ProbeStream>, Failure> {
        let data = std::fs::read(path).unwrap();
        let value = if data.starts_with(b"WEBVTT") {
            json!({"index": 0, "codec_type": "subtitle", "codec_name": "webvtt"})
        } else if data.windows(3).any(|w| w == b"aaa") {
            json!({"index": 0, "codec_type": "audio", "codec_name": "aac"})
        } else {
            json!({"index": 0, "codec_type": "video", "codec_name": "h264"})
        };
        Ok(vec![stream(value)])
    };
    let result = ws
        .analyze_with("master.m3u8", &ws.policy(), &probe)
        .unwrap();
    assert_eq!(
        result
            .tracks
            .iter()
            .map(|t| t.id.as_str())
            .collect::<Vec<_>>(),
        ["v000", "a000", "t000"]
    );
    assert_eq!(result.tracks[1].name, "English, AD");
    assert_eq!(result.tracks[1].language, "en");
    assert_eq!(result.tracks[2].group_id, "subs");
}

#[test]
fn map_and_implicit_byte_ranges() {
    let ws = Workspace::new();
    std::fs::write(
        ws.source.join("all.mp4"),
        b"\x00\x00\x00\x08ftyp\x00\x00\x00\x08moof\x00\x00\x00\x08moof",
    )
    .unwrap();
    std::fs::write(ws.source.join("master.m3u8"), "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MAP:URI=\"all.mp4\",BYTERANGE=\"8@0\"\n#EXT-X-BYTERANGE:8@8\n#EXTINF:1,\nall.mp4\n#EXT-X-BYTERANGE:8\n#EXTINF:1,\nall.mp4\n#EXT-X-ENDLIST\n").unwrap();
    let result = ws.analyze().unwrap();
    let track = &result.tracks[0];
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
    assert_eq!(track.segments.len(), 2);
}

#[test]
fn semantics_fail_closed() {
    for tag in [
        r#"#EXT-X-KEY:METHOD=AES-128,URI="secret.key""#,
        "#EXT-X-DISCONTINUITY",
        r#"#EXT-X-PART:DURATION=1,URI="part.ts""#,
        "#EXT-X-GAP",
        "#EXT-X-FUTURE-SEMANTIC:1",
    ] {
        let ws = Workspace::new();
        media(
            &ws.source,
            Some(&format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:6\n{tag}\n#EXTINF:6,\nsegment.ts\n#EXT-X-ENDLIST\n"
            )),
        );
        assert!(matches!(ws.analyze(), Err(Failure::Source(_))), "{tag}");
    }
}

#[test]
fn live_rejected() {
    let ws = Workspace::new();
    media(
        &ws.source,
        Some("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nsegment.ts\n"),
    );
    assert_eq!(token(ws.analyze()), "live-unsupported");
}

#[test]
fn bbb_zero_length_cue_pair_preserves_source_segments() {
    // A synthetic playlist splits at 25.375 s and has an
    // immediate zero-duration OUT/IN pair between two segments.
    let ws = Workspace::new();
    let body = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:1.375,\nsegment.ts\n#EXT-X-CUE-OUT:0.0\n#EXT-X-CUE-IN\n#EXTINF:4.625,\nsegment.ts\n#EXT-X-ENDLIST\n";
    media(&ws.source, Some(body));
    let result = ws.analyze().unwrap();
    assert!(result.admitted);
    let durations: Vec<f64> = result.tracks[0]
        .segments
        .iter()
        .map(|s| s.duration)
        .collect();
    assert_eq!(durations, [1.375, 4.625]);
    let original = std::fs::read(ws.source.join("segment.ts")).unwrap();
    assert!(
        result.tracks[0]
            .segments
            .iter()
            .all(|s| std::fs::read(&s.path).unwrap() == original)
    );
    let directory = result.snapshot_path.parent().unwrap();
    let frozen: Vec<Vec<u8>> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "m3u8"))
        .map(|path| std::fs::read(path).unwrap())
        .collect();
    assert!(frozen.contains(&body.as_bytes().to_vec()));
}

#[test]
fn active_or_unbalanced_ad_cues_still_fail_closed() {
    for cue in [
        "#EXT-X-CUE-OUT:30\n#EXT-X-CUE-IN",
        "#EXT-X-CUE-OUT:0.0",
        "#EXT-X-CUE-IN",
        "#EXT-X-CUE-OUT:NaN\n#EXT-X-CUE-IN",
        "#EXT-X-CUE-OUT:0.0\n#EXT-X-CUE-OUT:0.0\n#EXT-X-CUE-IN",
        "#EXT-X-CUE-IN:unexpected",
        "#EXT-X-CUE-OUT:0.0\n#EXTINF:1,\nsegment.ts\n#EXT-X-CUE-IN",
    ] {
        let ws = Workspace::new();
        media(
            &ws.source,
            Some(&format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nsegment.ts\n{cue}\n#EXTINF:6,\nsegment.ts\n#EXT-X-ENDLIST\n"
            )),
        );
        assert!(matches!(ws.analyze(), Err(Failure::Source(_))), "{cue}");
    }
}

#[test]
fn zero_cue_pair_requires_a_complete_segment_boundary() {
    for (before, after) in [
        ("", "#EXTINF:6,\nsegment.ts\n"),
        ("#EXTINF:6,\nsegment.ts\n#EXTINF:6,\n", "segment.ts\n"),
        (
            "#EXTINF:6,\nsegment.ts\n#EXT-X-BYTERANGE:188@0\n",
            "#EXTINF:6,\nsegment.ts\n",
        ),
        ("#EXTINF:6,\nsegment.ts\n", ""),
    ] {
        let ws = Workspace::new();
        media(
            &ws.source,
            Some(&format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:6\n{before}#EXT-X-CUE-OUT:0.0\n#EXT-X-CUE-IN\n{after}#EXT-X-ENDLIST\n"
            )),
        );
        assert!(
            matches!(ws.analyze(), Err(Failure::Source(_))),
            "{before:?} {after:?}"
        );
    }
}

#[test]
fn path_escape_and_range_bounds() {
    let ws = Workspace::new();
    std::fs::write(ws.path().join("outside.ts"), b"private").unwrap();
    media(
        &ws.source,
        Some("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\n../outside.ts\n#EXT-X-ENDLIST\n"),
    );
    assert_eq!(token(ws.analyze()), "source-not-allowed");
    media(
        &ws.source,
        Some(
            "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-BYTERANGE:9999@0\n#EXTINF:6,\nsegment.ts\n#EXT-X-ENDLIST\n",
        ),
    );
    assert_eq!(token(ws.analyze()), "invalid-byte-range");
}

#[test]
fn an_empty_byte_range_value_leaves_the_segment_whole_yet_counts_as_pending() {
    // `_range(...) if pending_range else None`: an empty value (with or without its colon) is
    // falsy, so the segment is fetched whole; it is still a pending value until a segment line.
    let ws = Workspace::new();
    media(
        &ws.source,
        Some(
            "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-BYTERANGE:\n#EXTINF:6,\nsegment.ts\n#EXTINF:6,\n#EXT-X-BYTERANGE\nsegment.ts\n#EXT-X-ENDLIST\n",
        ),
    );
    let result = ws.analyze().unwrap();
    assert_eq!(result.tracks[0].segments.len(), 2);
    assert!(
        result.tracks[0]
            .segments
            .iter()
            .all(|segment| segment.byte_range.is_none())
    );
    for (body, expected) in [
        (
            "#EXTINF:6,\nsegment.ts\n#EXT-X-BYTERANGE:\n",
            "invalid-playlist",
        ),
        (
            "#EXT-X-BYTERANGE:\n#EXT-X-BYTERANGE:\n#EXTINF:6,\nsegment.ts\n",
            "invalid-byte-range",
        ),
        (
            "#EXT-X-BYTERANGE:\n#EXTINF:6,\nsegment.ts\n#EXT-X-BYTERANGE:10\n#EXTINF:6,\nsegment.ts\n",
            "invalid-byte-range",
        ),
    ] {
        let ws = Workspace::new();
        media(
            &ws.source,
            Some(&format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:6\n{body}#EXT-X-ENDLIST\n"
            )),
        );
        assert_eq!(token(ws.analyze()), expected, "{body:?}");
    }
}

#[test]
fn a_nul_in_a_local_path_escapes_as_the_value_error_python_s_resolve_raises() {
    // `Path(unquote(path)).resolve`: `os.lstat` refuses an embedded NUL with a ValueError,
    // which neither `realpath` nor `_fetch` catches; the partial snapshot is removed.
    let ws = Workspace::new();
    media(
        &ws.source,
        Some("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nseg%00.ts\n#EXT-X-ENDLIST\n"),
    );
    assert_eq!(
        ws.analyze().unwrap_err(),
        Failure::Escaped(Escaped::new("ValueError"))
    );
    let result = Analysis::new(&default_probe).run(
        &SourceRequest::new(ws.source.join("a\0b.m3u8").to_string_lossy()),
        "nul",
        &ws.path().join("out"),
        &ws.policy(),
    );
    assert_eq!(
        result.unwrap_err(),
        Failure::Escaped(Escaped::new("ValueError"))
    );
    assert!(!ws.path().join("out/nul/clear/source").exists());
}

#[test]
fn a_directory_named_as_a_local_segment_cannot_be_opened_with_or_without_a_range() {
    // `path.open("rb")` refuses a directory (IsADirectoryError, an OSError) before a range is
    // compared with the size: `source-read-failed`, never `invalid-byte-range`.
    for range in ["#EXT-X-BYTERANGE:100000@0\n", "#EXT-X-BYTERANGE:1@0\n", ""] {
        let ws = Workspace::new();
        std::fs::create_dir(ws.source.join("sub")).unwrap();
        media(
            &ws.source,
            Some(&format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:6\n{range}#EXTINF:6,\nsub\n#EXT-X-ENDLIST\n"
            )),
        );
        assert_eq!(token(ws.analyze()), "source-read-failed", "{range:?}");
    }
}

#[test]
fn limits_and_secret_repr() {
    let ws = Workspace::new();
    media(&ws.source, None);
    let policy = FetchPolicy {
        max_total_bytes: 10,
        ..ws.policy()
    };
    assert_eq!(
        token(ws.analyze_with("master.m3u8", &policy, &default_probe)),
        "byte-budget-exceeded"
    );
    let secret = "https://user:password@example.test/master.m3u8?token=secret";
    let shown = format!("{:?}", SourceRequest::new(secret));
    assert!(!shown.contains(secret) && !shown.contains("password"));
    let policy = FetchPolicy {
        allowed_origins: vec!["https://example.test".into()],
        ..FetchPolicy::default()
    };
    let error = Analysis::new(&default_probe)
        .run(
            &SourceRequest::new(secret),
            "safe",
            &ws.path().join("out"),
            &policy,
        )
        .unwrap_err();
    let text = format!("{error} {error:?}");
    assert!(
        !text.contains("password") && !text.contains("token"),
        "{text}"
    );
    let with_headers = FetchPolicy {
        allowed_origins: vec!["https://example.test".into()],
        headers_by_origin: vec![(
            "https://example.test".into(),
            vec![("Authorization".into(), "sentinel-auth".into())],
        )],
        ..FetchPolicy::default()
    };
    assert!(!format!("{with_headers:?}").contains("sentinel"));
}

#[test]
fn duplicate_playlist_cycle_and_unknown_track() {
    let ws = Workspace::new();
    std::fs::write(
        ws.source.join("master.m3u8"),
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=10\nmaster.m3u8\n",
    )
    .unwrap();
    assert_eq!(token(ws.analyze()), "playlist-cycle");
    media(&ws.source, None);
    let probe = fixed(vec![
        json!({"index": 0, "codec_type": "video", "codec_name": "vp9"}),
    ]);
    assert_eq!(
        token(ws.analyze_with("master.m3u8", &ws.policy(), &probe)),
        "unsupported-source-track"
    );
}

#[test]
fn muxed_candidates_have_stable_indices_and_groups() {
    let ws = Workspace::new();
    media(&ws.source, None);
    let probe = fixed(vec![
        json!({"index": 0, "codec_type": "video", "codec_name": "h264"}),
        json!({"index": 1, "codec_type": "audio", "codec_name": "aac"}),
    ]);
    let result = ws
        .analyze_with("master.m3u8", &ws.policy(), &probe)
        .unwrap();
    let rows: Vec<(&str, i64, bool)> = result
        .tracks
        .iter()
        .map(|t| (t.id.as_str(), t.stream_index, t.muxed))
        .collect();
    assert_eq!(rows, [("v000", 0, true), ("a000", 1, true)]);
    assert_eq!(result.tracks[1].group_id, "muxed-audio");
}

#[test]
fn inband_audio_group_preserves_language_name_role() {
    let ws = Workspace::new();
    media(&ws.source, None);
    std::fs::copy(ws.source.join("master.m3u8"), ws.source.join("video.m3u8")).unwrap();
    let declaration = "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"main\",NAME=\"Japanese\",LANGUAGE=\"ja\",DEFAULT=YES,CHARACTERISTICS=\"public.accessibility.describes-video\"\n";
    std::fs::write(
        ws.source.join("master.m3u8"),
        format!(
            "#EXTM3U\n{declaration}#EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"main\"\nvideo.m3u8\n"
        ),
    )
    .unwrap();
    let probe = fixed(vec![
        json!({"index": 0, "codec_type": "video", "codec_name": "h264"}),
        json!({"index": 1, "codec_type": "audio", "codec_name": "aac"}),
    ]);
    let result = ws
        .analyze_with("master.m3u8", &ws.policy(), &probe)
        .unwrap();
    let audio = result
        .tracks
        .iter()
        .find(|t| t.content_type == "audio")
        .unwrap();
    assert_eq!(
        (
            audio.language.as_str(),
            audio.name.as_str(),
            audio.group_id.as_str(),
            audio.role.as_str()
        ),
        (
            "ja",
            "Japanese",
            "main",
            "public.accessibility.describes-video"
        )
    );
    assert!(audio.is_default && audio.autoselect);
    let restored = read_analysis(&result.snapshot_path).unwrap();
    assert!(restored.tracks[1].is_default && restored.tracks[1].autoselect);
}

#[test]
fn ambiguous_inband_audio_declarations_rejected() {
    let ws = Workspace::new();
    media(&ws.source, None);
    std::fs::copy(ws.source.join("master.m3u8"), ws.source.join("video.m3u8")).unwrap();
    std::fs::write(ws.source.join("master.m3u8"), "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"main\",NAME=\"Japanese\",LANGUAGE=\"ja\"\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"main\",NAME=\"English\",LANGUAGE=\"en\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"main\"\nvideo.m3u8\n").unwrap();
    assert_eq!(token(ws.analyze()), "ambiguous-source-topology");
}

#[test]
fn he_aac_profile_fails_instead_of_signaling_as_aac_lc() {
    let ws = Workspace::new();
    media(&ws.source, None);
    let probe = fixed(vec![
        json!({"index": 0, "codec_type": "audio", "codec_name": "aac", "profile": "HE-AAC"}),
    ]);
    assert_eq!(
        token(ws.analyze_with("master.m3u8", &ws.policy(), &probe)),
        "unsupported-aac-profile"
    );
}

#[test]
fn forced_subtitles_fail_instead_of_losing_selection_semantics() {
    let ws = Workspace::new();
    std::fs::write(ws.source.join("master.m3u8"), "#EXTM3U\n#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"Forced\",LANGUAGE=\"ja\",FORCED=YES,URI=\"subs.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=100,SUBTITLES=\"subs\"\nvideo.m3u8\n").unwrap();
    assert_eq!(token(ws.analyze()), "unsupported-forced-subtitles");
}

#[test]
fn invalid_rendition_selection_flags_rejected() {
    for attributes in [
        "DEFAULT=maybe",
        "AUTOSELECT=true",
        "DEFAULT=YES,AUTOSELECT=NO",
    ] {
        let ws = Workspace::new();
        std::fs::write(ws.source.join("master.m3u8"), format!("#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Japanese\",{attributes},URI=\"audio.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=100,AUDIO=\"audio\"\nvideo.m3u8\n")).unwrap();
        assert_eq!(token(ws.analyze()), "invalid-playlist", "{attributes}");
    }
}

#[test]
fn fetch_integer_limits_cannot_disable_bounds() {
    // Python passes nan, inf, 1.5 and True; a JSON policy cannot hold nan or inf (the reader
    // refuses the literal), and the typed struct cannot hold any of them.
    for field in [
        "max_total_bytes",
        "max_resource_bytes",
        "max_playlist_bytes",
        "max_playlists",
        "max_segments_per_track",
        "attempts",
        "min_free_disk_bytes",
    ] {
        for value in [
            json!(1.5),
            json!(true),
            json!(f64::MAX),
            json!(-1),
            json!("1"),
        ] {
            let error = FetchPolicy::from_json(&json!({ field: value })).unwrap_err();
            assert_eq!(error.token(), "invalid-fetch-policy", "{field} = {value}");
        }
    }
    for field in [
        "max_total_bytes",
        "max_resource_bytes",
        "max_playlist_bytes",
        "max_playlists",
        "max_segments_per_track",
        "attempts",
    ] {
        let error = FetchPolicy::from_json(&json!({ field: 0 })).unwrap_err();
        assert_eq!(error.token(), "invalid-fetch-policy", "{field} = 0");
    }
    assert!(FetchPolicy::from_json(&json!({"min_free_disk_bytes": 0})).is_ok());
    assert_eq!(
        FetchPolicy::from_json(&json!({"attempts": 6}))
            .unwrap_err()
            .token(),
        "invalid-fetch-policy"
    );
    assert_eq!(
        FetchPolicy::from_json(&json!({"unknown": 1}))
            .unwrap_err()
            .token(),
        "invalid-fetch-policy"
    );
}

#[test]
fn fetch_timeout_requires_finite_positive_number() {
    for timeout in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
        let policy = FetchPolicy {
            timeout,
            ..FetchPolicy::default()
        };
        assert_eq!(
            policy.validate().unwrap_err().token(),
            "invalid-fetch-policy",
            "{timeout}"
        );
    }
    for value in [json!(true), json!(0), json!(-1), json!("30")] {
        let error = FetchPolicy::from_json(&json!({ "timeout": value })).unwrap_err();
        assert_eq!(error.token(), "invalid-fetch-policy", "{value}");
    }
    assert_eq!(
        FetchPolicy::from_json(&json!({"timeout": 0.5}))
            .unwrap()
            .timeout,
        0.5
    );
    assert_eq!(
        FetchPolicy::from_json(&json!({"timeout": 2}))
            .unwrap()
            .timeout,
        2.0
    );
}

#[test]
fn each_resource_limit_fails_without_partial_snapshot() {
    let cases: [(Setting, &str); 3] = [
        (|p| p.max_playlist_bytes = 8, "playlist-too-large"),
        (|p| p.max_segments_per_track = 1, "segment-budget-exceeded"),
        (|p| p.max_resource_bytes = 8, "byte-budget-exceeded"),
    ];
    for (setting, classification) in cases {
        let ws = Workspace::new();
        media(
            &ws.source,
            Some(
                "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nsegment.ts\n#EXTINF:6,\nsegment.ts\n#EXT-X-ENDLIST\n",
            ),
        );
        let mut policy = ws.policy();
        setting(&mut policy);
        assert_eq!(
            token(ws.analyze_with("master.m3u8", &policy, &default_probe)),
            classification
        );
        assert!(!ws.path().join("output/test-asset/clear/source").exists());
    }
}

#[test]
fn invalid_and_unbounded_durations_rejected() {
    for duration in ["NaN", "Infinity", "1e99999999999", "0", "-1", "6.5"] {
        let ws = Workspace::new();
        media(
            &ws.source,
            Some(&format!(
                "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:{duration},\nsegment.ts\n#EXT-X-ENDLIST\n"
            )),
        );
        assert!(
            matches!(ws.analyze(), Err(Failure::Source(_))),
            "{duration}"
        );
    }
}

#[test]
fn container_is_detected_not_trusted_from_extension() {
    let ws = Workspace::new();
    media(&ws.source, None);
    std::fs::write(ws.source.join("segment.ts"), b"unrecognized container data").unwrap();
    assert_eq!(token(ws.analyze()), "unsupported-container");
}

#[test]
fn implicit_range_cannot_follow_different_resource() {
    let ws = Workspace::new();
    media(
        &ws.source,
        Some(
            "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-BYTERANGE:188@0\n#EXTINF:6,\nsegment.ts\n#EXT-X-BYTERANGE:188\n#EXTINF:6,\nother.ts\n#EXT-X-ENDLIST\n",
        ),
    );
    std::fs::copy(ws.source.join("segment.ts"), ws.source.join("other.ts")).unwrap();
    assert_eq!(token(ws.analyze()), "invalid-byte-range");
}

#[cfg(unix)]
#[test]
fn snapshot_metadata_tampering_and_symlink_output_rejected() {
    let ws = Workspace::new();
    media(&ws.source, None);
    let result = ws.analyze().unwrap();
    let mut data: Value =
        serde_json::from_slice(&std::fs::read(&result.snapshot_path).unwrap()).unwrap();
    data["tracks"][0]["codec"] = json!("hevc");
    std::fs::write(&result.snapshot_path, serde_json::to_string(&data).unwrap()).unwrap();
    assert_eq!(
        token(read_analysis(&result.snapshot_path)),
        "snapshot-changed"
    );
    let other = ws.path().join("external");
    std::fs::create_dir(&other).unwrap();
    std::fs::create_dir(ws.path().join("out")).unwrap();
    std::os::unix::fs::symlink(&other, ws.path().join("out/symlink")).unwrap();
    let result = Analysis::new(&default_probe).run(
        &SourceRequest::new(ws.source.join("master.m3u8").to_string_lossy()),
        "symlink",
        &ws.path().join("out"),
        &ws.policy(),
    );
    assert_eq!(token(result), "unsafe-output-path");
}

#[test]
fn concurrency_bounds_are_positive_bounded_integers() {
    for value in [0, 65] {
        let policy = FetchPolicy {
            connections: value,
            ..FetchPolicy::default()
        };
        assert_eq!(
            policy.validate().unwrap_err().token(),
            "invalid-fetch-policy",
            "connections {value}"
        );
        let policy = FetchPolicy {
            probe_workers: Some(value),
            ..FetchPolicy::default()
        };
        assert_eq!(
            policy.validate().unwrap_err().token(),
            "invalid-fetch-policy",
            "probe_workers {value}"
        );
    }
    for field in ["connections", "probe_workers"] {
        for value in [json!(0), json!(65), json!(1.5), json!(true)] {
            let error = FetchPolicy::from_json(&json!({ field: value })).unwrap_err();
            assert_eq!(error.token(), "invalid-fetch-policy", "{field} = {value}");
        }
    }
    let bounds = FetchPolicy::from_json(&json!({"connections": 64, "probe_workers": 64})).unwrap();
    assert_eq!((bounds.connections, bounds.probe_workers), (64, Some(64)));
}

#[test]
fn probe_workers_default_to_the_cpu_count_capped_at_thirty_two_and_connections_to_sixteen() {
    let policy = FetchPolicy::default();
    assert_eq!((policy.connections, policy.probe_workers), (16, None));
    assert_eq!(
        policy.effective_probe_workers(),
        drmr_core::concurrency::default_workers()
    );
    assert_eq!(
        FetchPolicy {
            probe_workers: Some(3),
            ..FetchPolicy::default()
        }
        .effective_probe_workers(),
        3
    );
}

#[test]
fn a_segment_listed_before_the_map_is_judged_without_an_init_as_the_sequential_analysis_did() {
    let mut first_ts = Vec::new();
    for _ in 0..3 {
        first_ts.extend(support::ts_packet("", b'0'));
    }
    let mut first_m4s = b"\x00\x00\x00\x08moof".to_vec();
    first_m4s.extend(std::iter::repeat_n(b'0', 180));
    for (data, ext, expected) in [
        (first_m4s, "m4s", "unsupported-container"),
        (first_ts, "ts", "changing-container-unsupported"),
    ] {
        let ws = Workspace::new();
        let src = ws.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let mut init = b"\x00\x00\x00\x08ftyp".to_vec();
        init.extend(std::iter::repeat_n(b'i', 8));
        std::fs::write(src.join("init.mp4"), init).unwrap();
        std::fs::write(src.join(format!("seg0.{ext}")), &data).unwrap();
        let mut second = b"\x00\x00\x00\x08moof".to_vec();
        second.extend(std::iter::repeat_n(b'1', 180));
        std::fs::write(src.join("seg1.m4s"), second).unwrap();
        let lines = [
            "#EXTM3U",
            "#EXT-X-TARGETDURATION:6",
            "#EXTINF:6,",
            &format!("seg0.{ext}"),
            "#EXT-X-MAP:URI=\"init.mp4\"",
            "#EXTINF:6,",
            "seg1.m4s",
            "#EXT-X-ENDLIST",
        ];
        std::fs::write(src.join("media.m3u8"), lines.join("\n") + "\n").unwrap();
        let policy = FetchPolicy {
            allowed_local_roots: vec![src.clone()],
            ..FetchPolicy::default()
        };
        let probe = fixed(support::video());
        let result = Analysis::new(&probe).run(
            &SourceRequest::new(src.join("media.m3u8").to_string_lossy()),
            "map-order",
            &ws.path().join("out"),
            &policy,
        );
        assert_eq!(token(result), expected, "{ext}");
        assert!(!ws.path().join("out/map-order/clear/source").exists());
    }
}

#[test]
fn concurrent_analysis_freezes_the_same_snapshot_as_the_sequential_one() {
    let ws = Workspace::new();
    support::ladder(&ws.source, 3, 12, true, true);
    let completed = Arc::new(Mutex::new(Vec::<String>::new()));
    let released = drmr_core::concurrency::Stop::new();
    // every playlist's first segment lands only after eight other resources have, so the
    // completion order differs from the submission order (which the ids must follow)
    let out_of_order = FnHooks {
        read: None,
        fetch: Some(Box::new({
            let (completed, released) = (Arc::clone(&completed), released.clone());
            move |fetcher, url, destination, range| {
                if url.ends_with("_0.ts") || url.ends_with("_0.m4s") {
                    assert!(released.wait(Some(std::time::Duration::from_secs(10))));
                }
                let result = fetcher.fetch(url, destination, range);
                let mut completed = completed.lock().unwrap();
                completed.push(url.to_string());
                if completed.len() == 8 {
                    released.set();
                }
                result
            }
        })),
    };
    let sequential = FetchPolicy {
        connections: 1,
        probe_workers: Some(1),
        ..ws.policy()
    };
    let parallel = FetchPolicy {
        connections: 8,
        probe_workers: Some(4),
        ..ws.policy()
    };
    let request = SourceRequest::new(ws.source.join("master.m3u8").to_string_lossy());
    let seq = Analysis::new(&support::ladder_probe)
        .run(&request, "ladder", &ws.path().join("seq"), &sequential)
        .unwrap();
    let collector = Collector::default();
    let par = Analysis::new(&support::ladder_probe)
        .hooks(&out_of_order)
        .log(&collector)
        .run(&request, "ladder", &ws.path().join("par"), &parallel)
        .unwrap();
    assert_eq!(
        (&par.snapshot_digest, &par.analysis_id),
        (&seq.snapshot_digest, &seq.analysis_id)
    );
    let public = |a: &drmr_core::source::SourceAnalysis| {
        a.tracks
            .iter()
            .map(|t| Value::Object(t.to_public_dict()))
            .collect::<Vec<_>>()
    };
    assert_eq!(public(&par), public(&seq));
    assert_eq!(
        par.tracks.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["v000", "v001", "v002", "v003", "a000"]
    );
    let read = |a: &drmr_core::source::SourceAnalysis| -> Value {
        serde_json::from_slice(&std::fs::read(&a.snapshot_path).unwrap()).unwrap()
    };
    let (seq_private, par_private) = (read(&seq), read(&par));
    assert_eq!(par_private["resources"], seq_private["resources"]);
    let resources = par_private["resources"].as_array().unwrap();
    let ids: Vec<&str> = resources
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    let expected: Vec<String> = (0..ids.len()).map(|i| format!("r{i:06}")).collect();
    assert_eq!(
        ids, expected,
        "ids are assigned in playlist order, not completion order"
    );
    let completed = completed.lock().unwrap();
    assert!(
        !completed[..8]
            .iter()
            .any(|url| url.ends_with("_0.ts") || url.ends_with("_0.m4s")),
        "the parallel run really completed out of order"
    );
    let kinds: Vec<&str> = resources[..4]
        .iter()
        .map(|r| r["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["playlist", "playlist", "segment", "segment"]);
    let par_dir = par.snapshot_path.parent().unwrap();
    let seq_dir = seq.snapshot_path.parent().unwrap();
    for row in resources {
        let name = row["path"].as_str().unwrap();
        assert_eq!(
            std::fs::read(par_dir.join(name)).unwrap(),
            std::fs::read(seq_dir.join(name)).unwrap()
        );
    }
    assert!(
        !std::fs::read_dir(par_dir).unwrap().any(|e| e
            .unwrap()
            .path()
            .extension()
            .is_some_and(|x| x == "part"))
    );
    verify_source_snapshot(&read_analysis(&par.snapshot_path).unwrap()).unwrap();
    // every playlist is announced before any media lands; each media playlist reports its own
    // checkpoints and completion; the master completes last with every track counted
    let events = events_of(&collector);
    let order = messages(&events);
    let last_started = order.iter().rposition(|m| m == "playlist-started").unwrap();
    assert!(
        last_started
            < order
                .iter()
                .position(|m| m == "segment-downloaded")
                .unwrap()
    );
    assert_eq!(order.iter().filter(|m| *m == "playlist-started").count(), 6);
    assert_eq!(
        order.iter().filter(|m| *m == "playlist-complete").count(),
        6
    );
    let master = &events[events.len() - 2];
    assert_eq!(
        (
            master["message"].as_str(),
            master["rendition_id"].as_str(),
            master["track_count"].as_u64()
        ),
        (Some("playlist-complete"), Some("r000000"), Some(5))
    );
    let renditions: std::collections::BTreeSet<String> = events
        .iter()
        .filter(|e| e["message"] == "segment-downloaded")
        .map(|e| e["rendition_id"].as_str().unwrap().to_string())
        .collect();
    for rendition in renditions {
        for name in ["segment-downloaded", "segment-probed"] {
            let indices: Vec<u64> = events
                .iter()
                .filter(|e| e["message"] == name && e["rendition_id"] == rendition.as_str())
                .map(|e| e["index"].as_u64().unwrap())
                .collect();
            assert_eq!(indices, [1, 10, 12], "{rendition} {name}");
        }
    }
}

#[test]
fn byte_budget_is_exact_under_concurrency_and_a_miss_leaves_no_partial_snapshot() {
    let ws = Workspace::new();
    support::ladder(&ws.source, 2, 40, false, false);
    // the master, two media playlists and eighty segments: every byte of the tree is fetched once
    let total: u64 = std::fs::read_dir(&ws.source)
        .unwrap()
        .map(|e| e.unwrap().metadata().unwrap().len())
        .sum();
    let request = SourceRequest::new(ws.source.join("master.m3u8").to_string_lossy());
    let exact = FetchPolicy {
        connections: 8,
        max_total_bytes: total,
        ..ws.policy()
    };
    let result = Analysis::new(&support::ladder_probe)
        .run(&request, "budget", &ws.path().join("exact"), &exact)
        .unwrap();
    assert!(
        result.admitted,
        "a budget equal to the bytes fetched is enough whatever the chunk interleaving"
    );
    let short = FetchPolicy {
        connections: 8,
        max_total_bytes: total - 1,
        ..ws.policy()
    };
    let result = Analysis::new(&support::ladder_probe).run(
        &request,
        "budget",
        &ws.path().join("short"),
        &short,
    );
    assert_eq!(token(result), "byte-budget-exceeded");
    assert!(!ws.path().join("short/budget/clear/source").exists());
}

#[test]
fn the_earliest_failure_in_playlist_order_among_the_work_that_ran_is_reported() {
    // : v1's download fails first on the clock; v0's inspection, already running,
    // fails afterwards and is the one reimplemented (playlist 0 comes before playlist 1).
    let ws = Workspace::new();
    support::ladder(&ws.source, 2, 2, false, false);
    let probing = drmr_core::concurrency::Stop::new();
    let v1_failed = drmr_core::concurrency::Stop::new();
    let hooks = FnHooks {
        read: None,
        fetch: Some(Box::new({
            let (probing, v1_failed) = (probing.clone(), v1_failed.clone());
            move |fetcher, url, destination, range| {
                if url.ends_with("v1_0.ts") {
                    assert!(probing.wait(Some(std::time::Duration::from_secs(10))));
                    v1_failed.set();
                    return Err(Failure::Source(drmr_core::source::SourceError::new(
                        "source-read-failed",
                    )));
                }
                fetcher.fetch(url, destination, range)
            }
        })),
    };
    let probe = {
        let (probing, v1_failed) = (probing.clone(), v1_failed.clone());
        move |path: &Path, _: Option<&Path>| -> Result<Vec<ProbeStream>, Failure> {
            if path.file_stem().unwrap() == "r000002" {
                probing.set();
                assert!(v1_failed.wait(Some(std::time::Duration::from_secs(10))));
                std::thread::sleep(std::time::Duration::from_millis(300));
                return Err(Failure::Source(drmr_core::source::SourceError::new(
                    "media-probe-failed",
                )));
            }
            Ok(streams(&[
                json!({"index": 0, "codec_type": "video", "codec_name": "h264"}),
            ]))
        }
    };
    let policy = FetchPolicy {
        connections: 4,
        probe_workers: Some(2),
        ..ws.policy()
    };
    let result = Analysis::new(&probe).hooks(&hooks).run(
        &SourceRequest::new(ws.source.join("master.m3u8").to_string_lossy()),
        "earliest",
        &ws.path().join("output"),
        &policy,
    );
    assert_eq!(token(result), "media-probe-failed");
    assert!(!ws.path().join("output/earliest/clear/source").exists());
}

#[test]
fn a_snapshot_directory_that_exists_is_never_replaced() {
    let ws = Workspace::new();
    media(&ws.source, None);
    ws.analyze().unwrap();
    assert_eq!(token(ws.analyze()), "source-snapshot-exists");
}

#[test]
fn an_invalid_asset_id_is_refused_before_anything_is_written() {
    let ws = Workspace::new();
    media(&ws.source, None);
    for asset_id in ["", ".", "..", "-x", "a/b", "a b", &"a".repeat(129), "é"] {
        let result = Analysis::new(&default_probe).run(
            &SourceRequest::new(ws.source.join("master.m3u8").to_string_lossy()),
            asset_id,
            &ws.path().join("output"),
            &ws.policy(),
        );
        assert_eq!(token(result), "invalid-asset-id", "{asset_id:?}");
    }
    assert!(!ws.path().join("output").exists());
}

#[test]
fn the_total_resource_budget_bounds_the_plan_when_the_policy_sets_one() {
    // New in the Rust tool : absent, nothing changes; set, the plan is refused
    // while it is built, before any media download.
    let ws = Workspace::new();
    media(
        &ws.source,
        Some(
            "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nsegment.ts\n#EXTINF:6,\nsegment.ts\n#EXT-X-ENDLIST\n",
        ),
    );
    let policy = FetchPolicy {
        max_total_resources: Some(2),
        ..ws.policy()
    };
    assert_eq!(
        token(ws.analyze_with("master.m3u8", &policy, &default_probe)),
        "resource-budget-exceeded"
    );
    let policy = FetchPolicy {
        max_total_resources: Some(3),
        ..ws.policy()
    };
    assert!(
        ws.analyze_with("master.m3u8", &policy, &default_probe)
            .is_ok()
    );
    assert_eq!(
        FetchPolicy::from_json(&json!({"max_total_resources": 0}))
            .unwrap_err()
            .token(),
        "invalid-fetch-policy"
    );
    assert_eq!(
        FetchPolicy::from_json(&json!({"max_total_resources": 9}))
            .unwrap()
            .max_total_resources,
        Some(9)
    );
}

/// `Path.resolve` of `CPython` 3.11.15 (the reference) raises `RuntimeError("Symlink loop …")` on
/// a symlink loop — the `stat` it runs after `posixpath.realpath` — and `hls_source` does not
/// classify it: `analyze-hls` reports `source-infrastructure-error` with exit 2 both for a master
/// URL through a loop and for an allowed root that is a loop (measured 2026-10-17 through
/// the CLI). 3.13 dropped that `stat`: there the same inputs give `source-read-failed`
/// and `source-not-allowed`, exit 1.
#[test]
#[cfg(unix)]
fn a_symlink_loop_escapes_as_3_11s_runtime_error() {
    let ws = Workspace::new();
    media(&ws.source, None);
    std::os::unix::fs::symlink("loop", ws.source.join("loop")).unwrap();
    let through_loop = ws.analyze_with("loop/master.m3u8", &ws.policy(), &default_probe);
    match through_loop {
        Err(Failure::Escaped(escaped)) => assert_eq!(escaped.class, "RuntimeError"),
        other => panic!("expected 3.11's RuntimeError, got {:?}", other.err()),
    }
    let root_is_a_loop = FetchPolicy {
        allowed_local_roots: vec![ws.source.join("loop")],
        ..FetchPolicy::default()
    };
    let via_root = ws.analyze_with("master.m3u8", &root_is_a_loop, &default_probe);
    match via_root {
        Err(Failure::Escaped(escaped)) => assert_eq!(escaped.class, "RuntimeError"),
        other => panic!("expected 3.11's RuntimeError, got {:?}", other.err()),
    }
}

/// `pypath::resolve` against `Path.resolve` of `CPython` 3.11.15 measured on this tree; 3.13.16
/// agrees on every case but the loops, where it returns the path as it is.
#[test]
#[cfg(unix)]
fn resolve_follows_3_11s_path_resolve() {
    use drmr_core::source::pypath::resolve;
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("dir/sub")).unwrap();
    std::fs::write(root.join("dir/file"), b"x").unwrap();
    symlink("dir", root.join("link")).unwrap();
    symlink("loop", root.join("loop")).unwrap();
    symlink("missing", root.join("dangling")).unwrap();
    symlink("l2", root.join("l1")).unwrap();
    symlink("l1", root.join("l2")).unwrap();
    let ok = |relative: &str| resolve(&root.join(relative)).unwrap();
    assert_eq!(ok("dir//sub/./"), root.join("dir/sub"));
    assert_eq!(ok("missing/x/../y"), root.join("missing/y"));
    assert_eq!(ok("link/sub/.."), root.join("dir"));
    assert_eq!(ok("dangling/x"), root.join("missing/x"));
    assert_eq!(ok("dir/file/x"), root.join("dir/file/x"));
    assert_eq!(ok("dir/file/.."), root.join("dir"));
    // the `stat` swallows every error but `ELOOP`; `loop/..` leaves the loop behind
    assert_eq!(ok("loop/.."), root);
    for looping in ["loop", "loop/x", "l1", "l1/x"] {
        assert_eq!(
            resolve(&root.join(looping)),
            Err(Escaped::new("RuntimeError")),
            "{looping}"
        );
    }
}

/// 3.11's `RuntimeError` of a symlink loop, escaping as itself.
fn runtime_error<T: std::fmt::Debug>(result: Result<T, Failure>) {
    match result {
        Err(Failure::Escaped(escaped)) => assert_eq!(escaped.class, "RuntimeError"),
        other => panic!("expected 3.11's RuntimeError, got {:?}", other.err()),
    }
}

/// The other three `resolve` sites of the clear-HLS path (`hls_source` 443, 1050 and 1320):
/// a `file:` URL through a loop, an output root that is a loop and `read_analysis` of a path
/// through a loop escape as 3.11's `RuntimeError` like the two sites above.
#[test]
#[cfg(unix)]
fn the_other_resolve_sites_escape_a_symlink_loop_the_same_way() {
    use drmr_core::source::pypath::as_uri;
    let ws = Workspace::new();
    media(&ws.source, None);
    std::os::unix::fs::symlink("loop", ws.source.join("loop")).unwrap();
    let analysis = Analysis::new(&default_probe);
    // 443: the master `file:` URL resolves through the loop
    let through_loop = as_uri(&ws.source.join("loop/master.m3u8")).unwrap();
    runtime_error(analysis.run(
        &SourceRequest::new(through_loop.as_str()),
        "test-asset",
        &ws.path().join("output"),
        &ws.policy(),
    ));
    // 1050: the output root is the loop
    std::os::unix::fs::symlink("output-loop", ws.path().join("output-loop")).unwrap();
    runtime_error(analysis.run(
        &SourceRequest::new(ws.source.join("master.m3u8").to_string_lossy()),
        "test-asset",
        &ws.path().join("output-loop"),
        &ws.policy(),
    ));
    // 1320: `read_analysis` of a path through the loop
    runtime_error(read_analysis(&ws.source.join("loop/analysis.json")));
}

/// A symlink loop encountered during resource confinement is rejected.
/// The error classification differs from a later filesystem read failure.
#[test]
#[cfg(unix)]
fn a_looping_recorded_file_is_snapshot_invalid_where_3_11_escapes() {
    let ws = Workspace::new();
    media(&ws.source, None);
    let result = ws.analyze().unwrap();
    let restored = read_analysis(&result.snapshot_path).unwrap();
    let loop_in_place = |path: &Path| {
        std::fs::remove_file(path).unwrap();
        std::os::unix::fs::symlink(path.file_name().unwrap(), path).unwrap();
    };
    // a recorded segment, through `read_analysis`
    loop_in_place(&result.tracks[0].segments[0].path);
    assert_eq!(
        token(read_analysis(&result.snapshot_path)),
        "snapshot-invalid"
    );
    // a recorded resource, through `verify_source_snapshot`
    let directory = result.snapshot_path.parent().unwrap();
    let data: Value =
        serde_json::from_slice(&std::fs::read(&result.snapshot_path).unwrap()).unwrap();
    loop_in_place(&directory.join(data["resources"][0]["path"].as_str().unwrap()));
    assert_eq!(token(verify_source_snapshot(&restored)), "snapshot-invalid");
}
