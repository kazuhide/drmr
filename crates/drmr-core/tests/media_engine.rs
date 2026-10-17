//! The subprocess media engine against the Python media helpers run on the same stub tools
//! (`fixtures/media/subprocess.python.json`): the same argv, the
//! same `command` / `tool-output` records (ts, duration and call id aside), the same results,
//! error classes and messages. The stubs are `sh` scripts printing canned, synthetic output; the
//! fixture carries their text. Native fixtures execute the same canned cases on every platform.
//! Record bounds remain checked against outputs larger than each limit.

#[path = "support/paths.rs"]
mod paths;
use paths::portable_path;

use drmr_core::canonical::Dumps;
use drmr_core::events::{Collector, Record};
use drmr_core::media::{
    FFMPEG, FRAMEMD5_MAX_OUTPUT_CHARS, MEDIA_MAX_OUTPUT_CHARS, MediaEngine, MediaError,
    PROBE_MAX_OUTPUT_CHARS, PacketInfo, Subprocess,
};
use drmr_core::toolrun::{DEFAULT_MAX_OUTPUT_CHARS, REDACTED, Secrets, ToolRunner};
use num_rational::BigRational;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn oracle() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/media/subprocess.python.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn spaced(hex: &str) -> String {
    (0..hex.len())
        .step_by(2)
        .map(|i| &hex[i..i + 2])
        .collect::<Vec<_>>()
        .join(" ")
}

/// The fixture's canned files and stub tools in `dir`.
fn materialize(dir: &Path, oracle: &Value) {
    let spec = &oracle["spec"];
    for (name, text) in spec["files"].as_object().unwrap() {
        std::fs::write(dir.join(name), text.as_str().unwrap()).unwrap();
    }
    std::fs::write(
        dir.join("pcm.bin"),
        hex::decode(spec["pcm_hex"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    let iv = oracle["iv"].as_str().unwrap();
    for name in spec["stubs"].as_object().unwrap().keys() {
        let mode = match name.as_str() {
            "ffmpeg" => drmr_test_support::Mode::Ffmpeg,
            "ffprobe" => drmr_test_support::Mode::Ffprobe,
            "mp4dump" => drmr_test_support::Mode::Mp4dump,
            _ => panic!("unknown fixture tool"),
        };
        let spec = drmr_test_support::ToolSpec {
            mode,
            ..Default::default()
        }
        .with("root", dir.to_str().unwrap())
        .with("iv_spaced", spaced(iv));
        drmr_test_support::install(&dir.join(name), &spec).unwrap();
    }
    std::fs::write(dir.join("media.mp4"), [0u8; 32]).unwrap();
}

struct Harness {
    _temp: tempfile::TempDir,
    dir: PathBuf,
    collector: Arc<Collector>,
    secrets: Arc<Secrets>,
    engine: Subprocess,
}

fn harness(oracle: &Value) -> Harness {
    let temp = tempfile::tempdir().unwrap();
    let dir = std::fs::canonicalize(temp.path()).unwrap();
    materialize(&dir, oracle);
    let secrets = Arc::new(Secrets::new());
    let collector = Arc::new(Collector::default());
    let mut engine = Subprocess::new(Arc::new(ToolRunner::new(
        secrets.clone(),
        collector.clone(),
    )));
    engine.ffmpeg_bin = drmr_test_support::program_path(&dir.join("ffmpeg"))
        .to_str()
        .unwrap()
        .to_string();
    engine.ffprobe_bin = drmr_test_support::program_path(&dir.join("ffprobe"))
        .to_str()
        .unwrap()
        .to_string();
    engine.mp4dump_bin = drmr_test_support::program_path(&dir.join("mp4dump"))
        .to_str()
        .unwrap()
        .to_string();
    Harness {
        _temp: temp,
        dir,
        collector,
        secrets,
        engine,
    }
}

/// A record as the reference keeps it: no ts / duration / call id, the work directory as `<dir>`,
/// the temporary tail as `<tmp>/tail.mp4`, argv[0] as its basename.
fn normalized(record: &Record, dir: &Path) -> Value {
    let dir = dir.to_str().unwrap();
    let mut out = serde_json::Map::new();
    for (key, value) in record {
        match key.as_str() {
            "ts" | "duration_s" | "call_id" => {}
            "argv" => {
                let mut argv: Vec<String> = value
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|token| native_text(token.as_str().unwrap(), dir))
                    .map(|token| {
                        if token.ends_with("/tail.mp4") || token.ends_with("\\tail.mp4") {
                            "<tmp>/tail.mp4".to_string()
                        } else {
                            token
                        }
                    })
                    .collect();
                let basename = Path::new(&argv[0]).file_name().unwrap().to_str().unwrap();
                argv[0] = if cfg!(windows) {
                    basename.strip_suffix(".exe").unwrap_or(basename)
                } else {
                    basename
                }
                .to_string();
                out.insert(key.clone(), json!(argv));
            }
            "tool" => {
                let name = Path::new(value.as_str().unwrap())
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap();
                let name = if cfg!(windows) {
                    name.strip_suffix(".exe").unwrap_or(name)
                } else {
                    name
                };
                out.insert(key.clone(), json!(name));
            }
            "stdout" | "stderr" => {
                out.insert(
                    key.clone(),
                    json!(native_text(value.as_str().unwrap(), dir)),
                );
            }
            _ => {
                out.insert(key.clone(), value.clone());
            }
        }
    }
    Value::Object(out)
}

fn outcome(result: Result<Value, MediaError>, dir: &Path) -> Value {
    match result {
        Ok(value) => json!({"ok": value}),
        Err(error) => json!({
            "error": error.python_class(),
            "message": native_text(&error.to_string(),dir.to_str().unwrap()),
        }),
    }
}

fn packet_json(info: &PacketInfo) -> Value {
    json!({"timescale": info.timescale, "pts": info.pts, "dts": info.dts,
           "durations": info.durations, "frame_rate": info.frame_rate})
}

fn shown(path: &Path, dir: &Path) -> Value {
    json!(native_text(path.to_str().unwrap(), dir.to_str().unwrap()))
}

fn fraction(numerator: i64, denominator: i64) -> BigRational {
    BigRational::new(numerator.into(), denominator.into())
}

/// The `hls_media` calls of the reference, by case name.
fn run_hls_media(name: &str, h: &Harness, iv: &str) -> Option<Result<Value, MediaError>> {
    let (e, d) = (&h.engine, h.dir.as_path());
    let media = d.join("media.mp4");
    let ffprobe = e.ffprobe_bin.clone();
    let path_text = media.to_str().unwrap().to_string();
    let normalize = |dest: &str, stream: i64, offset: BigRational, kind: &str| {
        e.normalize_segment(&media, &d.join(dest), stream, &offset, kind)
            .map(|p| shown(&p, d))
    };
    Some(match name {
        "probe_packets" => e.probe_packets(&media, 1).map(|p| packet_json(&p)),
        "probe_packets-missing-stream" => e.probe_packets(&media, 7).map(|p| packet_json(&p)),
        "packet_payloads" => e
            .packet_payloads(&media, 0)
            .map(|p| json!(p.iter().map(hex::encode).collect::<Vec<_>>())),
        "payload_hashes" => e.packet_payload_hashes(&media, 0).map(Value::from),
        "normalize_segment-video" => normalize("out-video.ts", 0, fraction(2, 1), "video"),
        "normalize_segment-audio" => normalize("out-audio.ts", 1, fraction(2, 1), "audio"),
        "normalize_segment-text" => normalize("out-text.ts", 0, fraction(2, 1), "text"),
        "normalize_segment-offset" => normalize("out.ts", 0, fraction(10, 3), "video"),
        "decoded_digest-video" => e.decoded_digest(&media, "video", 0).map(Value::from),
        "decoded_digest-audio" => e.decoded_digest(&media, "audio", 1).map(Value::from),
        "decoded_digest-failed" => e
            .decoded_digest(&d.join("broken.mp4"), "video", 0)
            .map(Value::from),
        "run_media-failed" => e
            .run_media(&[ffprobe, "-v".into(), "error".into(), path_text], true)
            .map(|_| Value::Null),
        "run_media-unavailable" => e
            .run_media(
                &[
                    d.join("absent-tool").to_str().unwrap().into(),
                    "-version".into(),
                ],
                true,
            )
            .map(|_| Value::Null),
        "run_media-unlogged" => e
            .run_media(&[ffprobe, "-show_data".into(), path_text], false)
            .map(|out| json!(out.len())),
        _ => return run_media_inspect(name, h, iv),
    })
}

/// The `media_inspect` calls of the reference, by case name.
fn run_media_inspect(name: &str, h: &Harness, iv: &str) -> Option<Result<Value, MediaError>> {
    let (e, d) = (&h.engine, h.dir.as_path());
    let media = d.join("media.mp4");
    let missing = d.join("missing.mp4");
    let decode = |input: &Path, out: &str, mode: &str| {
        e.decode_hls_media_playlist(input, &d.join(out), mode)
            .map(|p| shown(&p, d))
    };
    Some(match name {
        "compute_framemd5" => e.compute_framemd5(&media).map(Value::from),
        "compute_framemd5-failed" => e.compute_framemd5(&d.join("broken.mp4")).map(Value::from),
        "probe_segment_timing" => e.probe_segment_timing(&media).map(|t| json!({"duration_s": t.duration_s, "packet_count": t.packet_count})),
        "probe_segment_timing-failed" => e.probe_segment_timing(&missing).map(|_| Value::Null),
        "read_tenc_constant_iv_hex" => e.read_tenc_constant_iv_hex(&media).map(|found| json!(found.is_some_and(|v| v == iv))),
        "read_tenc_constant_iv_hex-failed" => e.read_tenc_constant_iv_hex(&missing).map(|_| Value::Null),
        "probe_init_segment" => e.probe_init_segment(&media).map(|p| json!([p.scheme_type, p.constant_iv_hex.is_some_and(|v| v == iv), p.default_kid_hex, p.per_sample_iv_size])),
        "read_fragment_boundary" => e.read_fragment_boundary(&media).map(|b| {
            b.map_or(Value::Null, |b| json!({"base_media_decode_time": b.base_media_decode_time, "sample_count": b.sample_count}))
        }),
        "measure_segment_durations_s" => e.measure_segment_durations_s(&media, &[media.clone(), media.clone()], 90_000).map(Value::from),
        "decode_media_to_pcm" => e.decode_media_to_pcm(&media, &d.join("pcm/out.pcm")).map(|p| shown(&p, d)),
        "decode_media_to_pcm-failed" => e.decode_media_to_pcm(&d.join("broken.mp4"), &d.join("out.pcm")).map(|p| shown(&p, d)),
        "decode_hls_media_playlist-pcm" => decode(&media, "decoded/out.pcm", "pcm"),
        "decode_hls_media_playlist-framemd5" => decode(&media, "decoded/out.framemd5", "framemd5"),
        "decode_hls_media_playlist-wav" => decode(&media, "decoded/out.wav", "wav"),
        "decode_hls_media_playlist-failed" => decode(&d.join("broken.m3u8"), "out.pcm", "pcm"),
        _ => return None,
    })
}

/// The cases whose Python outcome or record this implementation does not reproduce, with why.
fn adjust_expected(name: &str, expected: &mut Value, records: &mut [Value], iv: &str) {
    match name {
        // The failure text of a spawn error is toolrun's (P0): Rust prints the io::Error,
        // Python the OSError. Compared by prefix below.
        "run_media-unavailable" => {
            records[1]["stderr"] = json!("could not run: <os error>");
        }
        // Python's RuntimeError quotes mp4dump's stderr unscrubbed; this implementation scrubs the job's
        // secrets from it (IV values must never appear in error messages).
        "read_tenc_constant_iv_hex-failed" => {
            let message = expected["message"]
                .as_str()
                .unwrap()
                .replace(&spaced(iv), REDACTED);
            expected["message"] = json!(message);
        }
        _ => {}
    }
}

#[test]
fn every_call_site_runs_pythons_argv_and_emits_pythons_records() {
    let oracle = oracle();
    let iv = oracle["iv"].as_str().unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 29);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let h = harness(&oracle);
        // The fixture registers the IV for the mp4dump reads (the job's secret).
        if name.contains("tenc")
            || name.contains("init_segment")
            || name.contains("boundary")
            || name.starts_with("measure")
        {
            h.secrets.register([iv]);
        }
        let result = run_hls_media(name, &h, iv).unwrap_or_else(|| panic!("no call for {name}"));
        let actual = outcome(result, &h.dir);
        let records = h.collector.records();
        let mut actual_records: Vec<Value> =
            records.iter().map(|r| normalized(r, &h.dir)).collect();
        let mut expected = case["outcome"].clone();
        let mut expected_records: Vec<Value> = case["records"].as_array().unwrap().clone();
        adjust_expected(name, &mut expected, &mut expected_records, iv);
        if name == "run_media-unavailable" {
            let stderr = actual_records[1]["stderr"].as_str().unwrap().to_string();
            assert!(stderr.starts_with("could not run: "), "{stderr}");
            actual_records[1]["stderr"] = json!("could not run: <os error>");
        }
        assert_eq!(actual, expected, "{name}: outcome");
        let text = |values: &[Value]| {
            values
                .iter()
                .map(|v| Dumps::DEFAULT.dumps(v))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            text(&actual_records),
            text(&expected_records),
            "{name}: records (key order included)"
        );
        // Each command record and its output share a call id.
        for pair in records.chunks(2) {
            assert_eq!(pair[0]["call_id"], pair[1]["call_id"], "{name}");
            assert_eq!(
                (pair[0]["event"].as_str(), pair[1]["event"].as_str()),
                (Some("command"), Some("tool-output"))
            );
        }
        let log = serde_json::to_string(&records).unwrap();
        assert!(
            !log.contains(&spaced(iv)) && !log.contains(iv),
            "{name}: the IV never reaches a record"
        );
    }
}

/// A stub that prints `count` times the character `fill` (to stdout, or stderr).
fn chatty(dir: &Path, name: &str, count: usize, to_stderr: bool) -> String {
    let spec = drmr_test_support::ToolSpec::default().with(
        if to_stderr {
            "stderr_repeat"
        } else {
            "stdout_repeat"
        },
        count.to_string(),
    );
    drmr_test_support::install(&dir.join(name), &spec)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

fn last_output(collector: &Collector) -> Record {
    collector
        .records()
        .into_iter()
        .rev()
        .find(|r| r["event"] == "tool-output")
        .unwrap()
}

fn assert_bounded(record: &Record, key: &str, limit: usize, total: usize) {
    assert_eq!(record["truncated"], json!(true));
    let expected = format!(
        "{}\n… ({} characters not shown)",
        "x".repeat(limit),
        total - limit
    );
    assert_eq!(
        record[key].as_str().unwrap(),
        expected,
        "bounded at {limit}"
    );
}

#[test]
fn each_call_site_bounds_its_records_as_python_does() {
    // run_media: 64 Ki; compute_framemd5: 16 Ki; the ffprobe / mp4dump reads: 64 Ki; the
    // decodes: the 256 Ki default. Each stub prints more than its bound.
    assert_eq!(
        (
            MEDIA_MAX_OUTPUT_CHARS,
            FRAMEMD5_MAX_OUTPUT_CHARS,
            PROBE_MAX_OUTPUT_CHARS
        ),
        (65_536, 16_384, 65_536)
    );
    let oracle = oracle();
    let mut h = harness(&oracle);
    let dir = h.dir.clone();
    let media = dir.join("media.mp4");

    let tool = chatty(&dir, "ffprobe-chatty", 70_000, false);
    let out = h.engine.run_media(&[tool], true).unwrap();
    assert_eq!(out.len(), 70_000, "the caller's copy is complete");
    assert_bounded(
        &last_output(&h.collector),
        "stdout",
        MEDIA_MAX_OUTPUT_CHARS,
        70_000,
    );

    h.engine.ffmpeg_bin = chatty(&dir, "ffmpeg-chatty", 20_000, false);
    assert_eq!(h.engine.compute_framemd5(&media).unwrap().len(), 20_000);
    assert_bounded(
        &last_output(&h.collector),
        "stdout",
        FRAMEMD5_MAX_OUTPUT_CHARS,
        20_000,
    );

    h.engine.ffprobe_bin = chatty(&dir, "ffprobe-timing", 70_000, false);
    let error = h.engine.probe_segment_timing(&media).unwrap_err();
    assert_eq!(error.python_class(), "ValueError", "70,000 x is no JSON");
    assert_bounded(
        &last_output(&h.collector),
        "stdout",
        PROBE_MAX_OUTPUT_CHARS,
        70_000,
    );

    h.engine.mp4dump_bin = chatty(&dir, "mp4dump-chatty", 70_000, true);
    assert!(h.engine.read_fragment_boundary(&media).is_err());
    assert_bounded(
        &last_output(&h.collector),
        "stderr",
        PROBE_MAX_OUTPUT_CHARS,
        70_000,
    );

    h.engine.ffmpeg_bin = chatty(&dir, "ffmpeg-decode", 300_000, true);
    h.engine
        .decode_media_to_pcm(&media, &dir.join("out.pcm"))
        .unwrap();
    assert_bounded(
        &last_output(&h.collector),
        "stderr",
        DEFAULT_MAX_OUTPUT_CHARS,
        300_000,
    );
    h.engine
        .decode_hls_media_playlist(&media, &dir.join("out.pcm"), "pcm")
        .unwrap();
    assert_bounded(
        &last_output(&h.collector),
        "stderr",
        DEFAULT_MAX_OUTPUT_CHARS,
        300_000,
    );
}

#[test]
fn the_packet_payload_dump_is_consumed_never_logged() {
    let oracle = oracle();
    let h = harness(&oracle);
    let payloads = h
        .engine
        .packet_payloads(&h.dir.join("media.mp4"), 0)
        .unwrap();
    assert_eq!(payloads, vec![b"\x00\x0bHello world".to_vec()]);
    let records = h.collector.records();
    assert_eq!(records.len(), 2);
    assert!(
        records[0]["argv"]
            .as_array()
            .unwrap()
            .contains(&json!("-show_data"))
    );
    let stdout = records[1]["stdout"].as_str().unwrap();
    assert!(
        stdout.starts_with("(stdout not logged: ") && stdout.ends_with(" bytes consumed as data)"),
        "{stdout}"
    );
    let log = serde_json::to_string(&records).unwrap();
    assert!(
        !log.contains("Hello") && !log.contains(&hex::encode("Hello")) && !log.contains("6c6c 6f")
    );
}

#[test]
fn a_streamed_decode_records_the_command_and_its_completion_only() {
    let oracle = oracle();
    let h = harness(&oracle);
    let digest = h
        .engine
        .decoded_digest(&h.dir.join("media.mp4"), "video", 0)
        .unwrap();
    // The digest input: per frame line (comments and blanks skipped), its last two fields.
    let expected_input = "6144, c463c2fa2a6cf8a8519de1a7311848cb\n6144, 6feb2a7969f1527f661713bc47738e52\n6144,   0123456789abcdef0123456789abcdef\n";
    assert_eq!(
        digest,
        drmr_core::snapshot::sha256_hex(expected_input.as_bytes())
    );
    let records = h.collector.records();
    assert_eq!(records.len(), 2);
    assert_eq!(
        (records[1]["stdout"].as_str(), records[1]["stderr"].as_str()),
        (Some(""), Some(""))
    );
    assert_eq!(records[0]["tool"], json!(FFMPEG));

    // A tool that cannot start: the completion says so, the token is media-tool-unavailable.
    let mut engine = Subprocess::new(Arc::new(ToolRunner::new(
        h.secrets.clone(),
        h.collector.clone(),
    )));
    engine.ffmpeg_bin = h.dir.join("absent-ffmpeg").to_str().unwrap().to_string();
    let error = engine
        .decoded_digest(&h.dir.join("media.mp4"), "audio", 1)
        .unwrap_err();
    assert_eq!(error.hls_token(), Some("media-tool-unavailable"));
    let last = h.collector.records().pop().unwrap();
    assert_eq!(
        (
            last["returncode"].clone(),
            last["stderr"].clone(),
            last["level"].clone()
        ),
        (Value::Null, json!("could not run"), json!("error"))
    );
}

#[test]
fn the_engine_is_a_shareable_trait_object_for_the_segment_pools() {
    fn shareable<T: Send + Sync>() {}
    shareable::<Subprocess>();
    let oracle = oracle();
    let h = harness(&oracle);
    let media = h.dir.join("media.mp4");
    let engine: Arc<dyn MediaEngine> = Arc::new(h.engine);
    let threads: Vec<_> = (0..8)
        .map(|index| {
            let (engine, media) = (engine.clone(), media.clone());
            std::thread::spawn(move || {
                if index % 2 == 0 {
                    packet_json(&engine.probe_packets(&media, 1).unwrap())
                } else {
                    json!(engine.decoded_digest(&media, "audio", 1).unwrap())
                }
            })
        })
        .collect();
    let results: Vec<Value> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(results.iter().step_by(2).all(|r| *r == results[0]));
    assert!(results.iter().skip(1).step_by(2).all(|r| *r == results[1]));
    // Records of concurrent calls interleave; each command still pairs with one output by id.
    let records = h.collector.records();
    assert_eq!(records.len(), 16);
    let mut by_call: std::collections::BTreeMap<u64, Vec<String>> =
        std::collections::BTreeMap::new();
    for record in &records {
        by_call
            .entry(record["call_id"].as_u64().unwrap())
            .or_default()
            .push(record["event"].as_str().unwrap().to_string());
    }
    assert_eq!(by_call.len(), 8);
    assert!(
        by_call
            .values()
            .all(|events| events == &["command", "tool-output"])
    );
}

// Only strings containing the actual temporary root carry native path spelling.
fn native_text(value: &str, dir: &str) -> String {
    let portable = portable_path(Path::new(dir));
    let has_root = value.contains(dir) || value.contains(&portable);
    let normalized = value.replace(dir, "<dir>").replace(&portable, "<dir>");
    if cfg!(windows) && has_root {
        normalized.replace('\\', "/")
    } else {
        normalized
    }
}
