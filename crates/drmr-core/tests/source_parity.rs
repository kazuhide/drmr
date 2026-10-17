//! Synthetic source snapshots, URL handling, numeric compatibility, and media mapping.
#![allow(clippy::unnecessary_wraps, clippy::too_many_lines)]

mod support;

use drmr_core::canonical::{Dumps, python_float_repr};
use drmr_core::events::Collector;
use drmr_core::snapshot::{analysis_id, sha256_hex, snapshot_digest};
use drmr_core::source::fsum::{fsum, sequential_sum};
use drmr_core::source::netaddr::is_global;
use drmr_core::source::pypath::{as_uri, resolve};
use drmr_core::source::pyurl::{parse_ip, quote_from_bytes, unquote, urljoin, urlsplit};
use drmr_core::source::{
    Escaped, Failure, FetchPolicy, ProbeStream, Prober, SourceRequest, child_url, origin,
    read_analysis,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use support::hls_fixture::{build_bbb_like_asset, files_under, size_rank};
use support::{Analysis, default_probe, ladder, ladder_probe, media, stream, streams, ts_packet};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/hls_source")
        .join(name)
}

fn load(name: &str) -> Value {
    serde_json::from_slice(&std::fs::read(fixture(name)).unwrap()).unwrap()
}

fn content_probe(path: &Path, _: Option<&Path>) -> Result<Vec<ProbeStream>, Failure> {
    let data = std::fs::read(path).unwrap();
    let value = if data.starts_with(b"WEBVTT") {
        json!({"index": 0, "codec_type": "subtitle", "codec_name": "webvtt"})
    } else if data.windows(3).any(|w| w == b"aaa") {
        json!({"index": 0, "codec_type": "audio", "codec_name": "aac"})
    } else {
        json!({"index": 0, "codec_type": "video", "codec_name": "h264"})
    };
    Ok(vec![stream(value)])
}

fn muxed_probe(_: &Path, _: Option<&Path>) -> Result<Vec<ProbeStream>, Failure> {
    Ok(streams(&[
        json!({"index": 0, "codec_type": "video", "codec_name": "h264"}),
        json!({"index": 1, "codec_type": "audio", "codec_name": "aac", "profile": "LC"}),
    ]))
}

const FRACTIONS: [&str; 7] = [
    "5.994666666666666",
    "5.994666666666666",
    "6.016",
    "6.006",
    "0.04",
    "6.4999",
    "2.002",
];

/// The input trees of `oracle`'s cases, written the same way.
fn build(case: &str, root: &Path) -> &'static str {
    match case {
        "ladder" => ladder(root, 3, 12, true, true),
        "cue" => media(
            root,
            Some(
                "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:1.375,\nsegment.ts\n#EXT-X-CUE-OUT:0.0\n#EXT-X-CUE-IN\n#EXTINF:4.625,\nsegment.ts\n#EXT-X-ENDLIST\n",
            ),
        ),
        "master" => {
            std::fs::write(root.join("master.m3u8"), "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"English, AD\",LANGUAGE=\"en\",CHARACTERISTICS=\"public.accessibility.describes-video\",URI=\"audio.m3u8\"\n#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"Japanese\",LANGUAGE=\"ja\",URI=\"text.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=400000,CODECS=\"avc1.64001e,mp4a.40.2\",RESOLUTION=640x360,AUDIO=\"audio\",SUBTITLES=\"subs\"\nvideo.m3u8\n").unwrap();
            for (name, segment) in [
                ("video", "segment.ts"),
                ("audio", "audio.ts"),
                ("text", "text.vtt"),
            ] {
                std::fs::write(
                    root.join(format!("{name}.m3u8")),
                    format!(
                        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\n{segment}\n#EXT-X-ENDLIST\n"
                    ),
                )
                .unwrap();
            }
            std::fs::write(root.join("segment.ts"), ts_packet("", b'x')).unwrap();
            std::fs::write(root.join("audio.ts"), ts_packet("", b'a')).unwrap();
            std::fs::write(
                root.join("text.vtt"),
                b"WEBVTT\n\n00:00.000 --> 00:01.000\nhello\n",
            )
            .unwrap();
        }
        "byterange" => {
            std::fs::write(
                root.join("all.mp4"),
                b"\x00\x00\x00\x08ftyp\x00\x00\x00\x08moof\x00\x00\x00\x08moof",
            )
            .unwrap();
            std::fs::write(root.join("master.m3u8"), "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MAP:URI=\"all.mp4\",BYTERANGE=\"8@0\"\n#EXT-X-BYTERANGE:8@8\n#EXTINF:1,\nall.mp4\n#EXT-X-BYTERANGE:8\n#EXTINF:1,\nall.mp4\n#EXT-X-ENDLIST\n").unwrap();
        }
        "emptyrange" => media(
            root,
            Some(
                "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-BYTERANGE:\n#EXTINF:6,\nsegment.ts\n#EXTINF:6,\n#EXT-X-BYTERANGE\nsegment.ts\n#EXT-X-ENDLIST\n",
            ),
        ),
        "inband" => {
            media(root, None);
            std::fs::copy(root.join("master.m3u8"), root.join("video.m3u8")).unwrap();
            std::fs::write(root.join("master.m3u8"), "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"main\",NAME=\"Japanese\",LANGUAGE=\"ja\",DEFAULT=YES,CHARACTERISTICS=\"public.accessibility.describes-video\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"main\"\nvideo.m3u8\n").unwrap();
        }
        "fractional" => {
            let mut lines: Vec<String> = [
                "#EXTM3U",
                "#EXT-X-VERSION:3",
                "#EXT-X-TARGETDURATION:6",
                "#EXT-X-MEDIA-SEQUENCE:0",
                "#EXT-X-PLAYLIST-TYPE:VOD",
            ]
            .iter()
            .map(ToString::to_string)
            .collect();
            for (index, duration) in FRACTIONS.iter().enumerate() {
                lines.push(format!("#EXTINF:{duration},"));
                lines.push(format!("seg {index}.ts"));
                std::fs::write(
                    root.join(format!("seg {index}.ts")),
                    ts_packet(&index.to_string(), b'y'),
                )
                .unwrap();
            }
            lines.push("#EXT-X-ENDLIST".into());
            std::fs::write(root.join("media.m3u8"), lines.join("\n") + "\n").unwrap();
            return "media.m3u8";
        }
        _ => unreachable!("{case}"),
    }
    "master.m3u8"
}

fn prober(case: &str) -> &'static dyn Prober {
    match case {
        "ladder" => &ladder_probe,
        "master" => &content_probe,
        "inband" => &muxed_probe,
        _ => &default_probe,
    }
}

/// Progress records as a multiset: a media playlist's `playlist-complete` carries the running
/// track total in completion order, which concurrency reorders; every other field is kept.
fn normalized(events: &[Value], last_kept: usize) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    let count = events.len();
    for (index, event) in events.iter().enumerate() {
        let mut event = event.clone();
        if event["message"] == "playlist-complete" && index + last_kept < count {
            event.as_object_mut().unwrap().remove("track_count");
        }
        *counts.entry(Dumps::SORTED.dumps(&event)).or_insert(0) += 1;
    }
    counts
}

#[test]
fn every_synthetic_case_freezes_the_analysis_python_freezes_byte_for_byte() {
    for case in [
        "ladder",
        "cue",
        "master",
        "byterange",
        "inband",
        "fractional",
        "emptyrange",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("input");
        std::fs::create_dir(&root).unwrap();
        let entry = build(case, &root);
        let collector = Collector::default();
        let policy = FetchPolicy {
            allowed_local_roots: vec![root.clone()],
            probe_workers: Some(1),
            ..FetchPolicy::default()
        };
        let result = Analysis::new(prober(case))
            .log(&collector)
            .run(
                &SourceRequest::new(root.join(entry).to_string_lossy()),
                case,
                &tmp.path().join("output"),
                &policy,
            )
            .unwrap_or_else(|error| panic!("{case}: {error:?}"));
        // The reference fixture's record with this run's root in place of @ROOT@, and its digest and id
        // recomputed: the canonical bytes are Python's when the two records are equal.
        let prefix = as_uri(&resolve(&root).unwrap()).unwrap();
        let text = std::fs::read_to_string(fixture(&format!("analysis/{case}.json"))).unwrap();
        let mut expected: Value = serde_json::from_str(&text.replace("@ROOT@", &prefix)).unwrap();
        let map = expected.as_object_mut().unwrap();
        let recorded_digest = map.shift_remove("snapshot_digest").unwrap();
        let recorded_id = map.shift_remove("analysis_id").unwrap();
        let digest = snapshot_digest(&expected);
        expected["snapshot_digest"] = json!(digest);
        expected["analysis_id"] = json!(analysis_id(&digest));
        let written = std::fs::read_to_string(&result.snapshot_path).unwrap();
        assert_eq!(
            written,
            Dumps::SORTED.dumps(&expected),
            "{case}: analysis.json bytes"
        );
        assert_eq!(
            (result.snapshot_digest.as_str(), result.analysis_id.as_str()),
            (digest.as_str(), analysis_id(&digest).as_str())
        );
        if !text.contains("@ROOT@") || prefix == "@ROOT@" {
            unreachable!("the record names its root");
        }
        let _ = (recorded_digest, recorded_id); // the reference fixture's own (its root differed)
        // The public record as the API serves it: Python's key order and number formatting.
        let public = Dumps::DEFAULT
            .dumps(&Value::Object(result.to_public_dict()))
            .replace(&result.snapshot_digest, "@DIGEST@")
            .replace(&result.analysis_id, "@ID@");
        let python_public =
            std::fs::read_to_string(fixture(&format!("public/{case}.json"))).unwrap();
        assert_eq!(public, python_public.trim_end(), "{case}: to_public_dict");
        // The progress records Python emitted for the same source.
        let python = load(&format!("events/{case}.json"));
        let python = python.as_array().unwrap();
        let rust: Vec<Value> = collector.records().into_iter().map(Value::Object).collect();
        let terminal = if matches!(case, "fractional" | "cue" | "byterange" | "emptyrange") {
            1
        } else {
            2
        };
        assert_eq!(
            normalized(&rust, terminal),
            normalized(python, terminal),
            "{case}: progress records"
        );
        assert_eq!(rust[..2], python[..2], "{case}: the first records in order");
        assert_eq!(
            rust[rust.len() - terminal..],
            python[python.len() - terminal..],
            "{case}: the terminal records in order"
        );
        let restored = read_analysis(&result.snapshot_path).unwrap();
        assert_eq!(restored.analysis_id, result.analysis_id, "{case}");
    }
}

#[test]
fn a_redirected_remote_source_freezes_the_record_python_freezes() {
    // The effective URL after the redirect (the playlist's), children joined against it
    // (relative with a query, absolute path, `..`): the bytes of the record, origin aside.
    let media = b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:5.005,\nseg0.ts?tok=a\n#EXTINF:4.004,\n/abs/seg1.ts\n#EXTINF:1.5,\n../up/seg2.ts\n#EXT-X-ENDLIST\n";
    let server = support::server::TestServer::routes(vec![
        (
            "/start",
            302,
            vec![("Location", "/hls/v1/media.m3u8?sig=1")],
            Vec::new(),
        ),
        ("/hls/v1/media.m3u8?sig=1", 200, vec![], media.to_vec()),
        ("/hls/v1/seg0.ts?tok=a", 200, vec![], ts_packet("0", b'x')),
        ("/abs/seg1.ts", 200, vec![], ts_packet("1", b'x')),
        ("/hls/up/seg2.ts", 200, vec![], ts_packet("2", b'x')),
    ]);
    let tmp = tempfile::tempdir().unwrap();
    let probe = support::fixed(vec![
        json!({"index": 0, "codec_type": "video", "codec_name": "h264", "width": 1, "height": 2}),
    ]);
    let result = Analysis::new(&probe)
        .run(
            &SourceRequest::new(format!("{}/start", server.origin)),
            "remote",
            tmp.path(),
            &support::remote_policy(&server.origin),
        )
        .unwrap();
    let text = std::fs::read_to_string(fixture("analysis/remote.json")).unwrap();
    let mut expected: Value =
        serde_json::from_str(&text.replace("@ORIGIN@", &server.origin)).unwrap();
    let map = expected.as_object_mut().unwrap();
    map.shift_remove("snapshot_digest");
    map.shift_remove("analysis_id");
    let digest = snapshot_digest(&expected);
    expected["snapshot_digest"] = json!(digest);
    expected["analysis_id"] = json!(analysis_id(&digest));
    let written = std::fs::read_to_string(&result.snapshot_path).unwrap();
    assert_eq!(written, Dumps::SORTED.dumps(&expected));
    assert_eq!(
        server.paths().len(),
        5,
        "one request per resource, the redirect included"
    );
}

fn attempt_string(result: Result<String, Failure>) -> Value {
    match result {
        Ok(text) => json!({"ok": text}),
        Err(Failure::Source(error)) => json!({"source_error": error.token()}),
        Err(Failure::Escaped(Escaped {
            class: "ValueError",
        })) => json!({"value_error": true}),
        Err(other) => panic!("{other:?}"),
    }
}

#[test]
fn urljoin_and_child_url_match_python() {
    let data = load("urls.json");
    for case in data["join"].as_array().unwrap() {
        let (base, reference) = (
            case["base"].as_str().unwrap(),
            case["ref"].as_str().unwrap(),
        );
        let joined = match urljoin(base, reference) {
            Ok(text) => json!({"ok": text}),
            Err(_) => json!({"value_error": true}),
        };
        assert_eq!(joined, case["urljoin"], "urljoin({base:?}, {reference:?})");
        assert_eq!(
            attempt_string(child_url(base, reference)),
            case["child_url"],
            "_child_url({base:?}, {reference:?})"
        );
    }
}

#[test]
fn urlsplit_origin_and_unquote_match_python() {
    let data = load("urls.json");
    for case in data["split"].as_array().unwrap() {
        let url = case["url"].as_str().unwrap();
        let split = match urlsplit(url, "") {
            Ok(parts) => json!({"ok": {
                "scheme": parts.scheme, "netloc": parts.netloc, "path": parts.path,
                "query": parts.query, "fragment": parts.fragment, "hostname": parts.hostname(),
                "username": parts.username(), "password": parts.password(),
                "port": match parts.port() { Ok(port) => json!(port), Err(_) => json!("ValueError") },
            }}),
            Err(_) => json!({"value_error": true}),
        };
        assert_eq!(split, case["split"], "urlsplit({url:?})");
        let origin = match origin(url) {
            Ok(text) => json!({"ok": text}),
            Err(error) => json!({"source_error": error.token()}),
        };
        assert_eq!(origin, case["origin"], "_origin({url:?})");
        let unquoted = match urlsplit(url, "") {
            Ok(parts) => json!({"ok": unquote(&parts.path)}),
            Err(_) => json!({"value_error": true}),
        };
        assert_eq!(unquoted, case["unquote_path"], "unquote({url:?})");
    }
}

#[test]
fn fsum_and_the_legacy_sum_match_python_for_every_case() {
    let parse = |text: &Value| -> f64 { text.as_str().unwrap().parse().unwrap() };
    for case in load("fsum.json").as_array().unwrap() {
        let values: Vec<f64> = case["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(parse)
            .collect();
        let total = fsum(values.iter().copied()).unwrap();
        assert_eq!(
            total.to_bits(),
            parse(&case["fsum"]).to_bits(),
            "fsum({values:?})"
        );
        assert_eq!(python_float_repr(total), case["fsum"].as_str().unwrap());
        let legacy = sequential_sum(values.iter().copied());
        assert_eq!(
            legacy.to_bits(),
            parse(&case["legacy"]).to_bits(),
            "legacy({values:?})"
        );
    }
    // repr(math.fsum([6.006] * 10000)) and the sequential total, from CPython 3.11.
    let long = vec![6.006; 10_000];
    assert_eq!(
        python_float_repr(fsum(long.iter().copied()).unwrap()),
        "60060.0"
    );
    assert_eq!(
        python_float_repr(sequential_sum(long.iter().copied())),
        "60060.00000000825"
    );
    let drift = 5.994_666_666_666_666;
    let bbb = [drift, drift, 6.016]
        .into_iter()
        .chain(std::iter::repeat_n(drift, 97));
    assert_eq!(python_float_repr(fsum(bbb).unwrap()), "599.4879999999999");
}

#[test]
fn fsum_reports_what_python_raises() {
    use drmr_core::source::fsum::FsumError;
    assert_eq!(fsum([1e308, 1e308]), Err(FsumError::Overflow));
    assert_eq!(
        fsum([f64::INFINITY, f64::NEG_INFINITY]),
        Err(FsumError::InfMinusInf)
    );
    assert_eq!(fsum([f64::INFINITY, 1.0]), Ok(f64::INFINITY));
    assert!(fsum([f64::NAN, 1.0]).unwrap().is_nan());
    assert_eq!(fsum(std::iter::empty()), Ok(0.0));
}

#[test]
fn is_global_matches_python() {
    for case in load("is_global.json").as_array().unwrap() {
        let address = case["address"].as_str().unwrap();
        let ip = parse_ip(address).unwrap();
        assert_eq!(
            is_global(ip),
            case["global"].as_bool().unwrap(),
            "{address}"
        );
    }
}

#[test]
fn as_uri_and_quote_match_python() {
    for case in load("as_uri.json").as_array().unwrap() {
        let path = case["path"].as_str().unwrap();
        assert_eq!(
            as_uri(Path::new(path)).unwrap(),
            case["as_uri"].as_str().unwrap(),
            "{path}"
        );
        assert_eq!(
            quote_from_bytes(path.as_bytes()),
            case["quote"].as_str().unwrap(),
            "{path}"
        );
    }
}

#[test]
fn the_hls_fixture_writes_the_tree_the_python_builder_writes() {
    let tmp = tempfile::tempdir().unwrap();
    build_bbb_like_asset(tmp.path());
    let rows: Vec<Value> = files_under(tmp.path())
        .into_iter()
        .map(|path| {
            let data = std::fs::read(&path).unwrap();
            let relative = path
                .strip_prefix(tmp.path())
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            json!({"path": relative, "size": data.len(), "sha256": sha256_hex(&data)})
        })
        .collect();
    let mut expected = load("hls_fixture_tree.json").as_array().unwrap().clone();
    let mut rows = rows;
    let key = |row: &Value| row["path"].as_str().unwrap().to_string();
    rows.sort_by_key(key);
    expected.sort_by_key(key);
    assert_eq!(rows, expected);
}

#[test]
fn the_bbb_like_asset_reproduces_the_audio_inversion_the_size_rank_must_resolve() {
    let tmp = tempfile::tempdir().unwrap();
    let asset = build_bbb_like_asset(tmp.path());
    // fMP4 audio: group index 0 is the larger group, so index order and size order disagree.
    let groups = ["ha-fmp4-0", "ha-fmp4-1"];
    let sizes: Vec<u64> = groups
        .iter()
        .map(|uuid| asset.on_disk_total_size(&asset.media_playlist_path(uuid, "audio")))
        .collect();
    assert_eq!(sizes, [1902 * 3, 470 * 3]);
    assert_eq!(
        size_rank(&sizes).unwrap(),
        [1, 0],
        "AUDIO-1 ranks first by size"
    );
    let dash: Vec<u64> = ["d-aud-64k", "d-aud-256k"]
        .iter()
        .map(|id| {
            asset
                .rendition_by_id(id)
                .segment_paths
                .iter()
                .map(|p| std::fs::metadata(p).unwrap().len())
                .sum()
        })
        .collect();
    assert_eq!(size_rank(&dash).unwrap(), [0, 1]);
    // The rank join pairs fmp4_AUDIO-1 with the 64k rendition and fmp4_AUDIO-0 with 256k; an
    // index-wise join would have mapped audio backwards.
    let join: Vec<(&str, &str)> = size_rank(&sizes)
        .unwrap()
        .into_iter()
        .zip(size_rank(&dash).unwrap())
        .map(|(h, d)| (groups[h], ["d-aud-64k", "d-aud-256k"][d]))
        .collect();
    assert_eq!(
        join,
        [("ha-fmp4-1", "d-aud-64k"), ("ha-fmp4-0", "d-aud-256k")]
    );
    assert!(size_rank(&[5, 5]).is_err(), "a tie cannot be ranked");
    assert_eq!(asset.variant_specs.len(), 6);
    assert_eq!(asset.audio_specs.len(), 4);
}
