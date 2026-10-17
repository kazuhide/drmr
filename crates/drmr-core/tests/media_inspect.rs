//! Media probing and decoded-content inspection using synthetic media.

use drmr_core::events::{Collector, NoSink, Sink};
use drmr_core::media::{
    HexSecret, InitProbe, MediaEngine, MediaError, Subprocess, extract_hls_iv_hex_values,
    localize_hls_master_http, localize_hls_media_playlist, localize_hls_media_playlist_http,
    parse_fragment_boundary, parse_init_probe, parse_tenc_constant_iv_hex, python,
    write_raw_key_file,
};
use drmr_core::toolrun::{REDACTED, Secrets, ToolRunner};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "10101010101010101010101010101010";
const IV: &str = "20202020202020202020202020202020";

fn on_path(tool: &str) -> bool {
    let found = std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(tool).is_file()));
    if !found {
        eprintln!("skipped: {tool} not on PATH");
    }
    found
}

fn engine_with(sink: Arc<dyn Sink>, secrets: Arc<Secrets>) -> Subprocess {
    Subprocess::new(Arc::new(ToolRunner::new(secrets, sink)))
}

fn engine() -> Subprocess {
    engine_with(Arc::new(NoSink), Arc::new(Secrets::new()))
}

fn run(argv: &[&str]) {
    let status = std::process::Command::new(argv[0])
        .args(&argv[1..])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "{argv:?}");
}

/// `_make_test_video`: a tiny lavfi clip (64x64, 5 fps).
fn make_test_video(path: &Path, pattern: &str, duration: f64) -> PathBuf {
    let source = format!("{pattern}=duration={duration}:size=64x64:rate=5");
    run(&[
        "ffmpeg",
        "-v",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &source,
        "-pix_fmt",
        "yuv420p",
        path.to_str().unwrap(),
    ]);
    path.to_path_buf()
}

fn bracketed(hex: &str) -> String {
    let pairs: Vec<&str> = (0..hex.len()).step_by(2).map(|i| &hex[i..i + 2]).collect();
    format!("[{}]", pairs.join(" "))
}

// ---- compute_framemd5 ----

#[test]
fn compute_framemd5_identical_content_matches() {
    if !on_path("ffmpeg") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let a = make_test_video(&dir.path().join("a.mp4"), "testsrc", 1.0);
    let b = dir.path().join("b.mp4");
    std::fs::copy(&a, &b).unwrap();
    let engine = engine();
    assert_eq!(
        engine.compute_framemd5(&a).unwrap(),
        engine.compute_framemd5(&b).unwrap()
    );
}

#[test]
fn compute_framemd5_different_content_differs() {
    if !on_path("ffmpeg") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let a = make_test_video(&dir.path().join("a.mp4"), "testsrc", 1.0);
    let b = make_test_video(&dir.path().join("b.mp4"), "testsrc2", 1.0);
    let engine = engine();
    assert_ne!(
        engine.compute_framemd5(&a).unwrap(),
        engine.compute_framemd5(&b).unwrap()
    );
}

#[test]
fn compute_framemd5_raises_on_unreadable_file() {
    if !on_path("ffmpeg") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let bogus = dir.path().join("not_a_video.mp4");
    std::fs::write(&bogus, b"not a real mp4 at all").unwrap();
    let error = engine().compute_framemd5(&bogus).unwrap_err();
    assert!(error.is_runtime_error(), "{error:?}");
    let prefix = format!(
        "ffmpeg framemd5 failed for {}: ",
        python::path_str(&bogus).unwrap()
    );
    assert!(error.to_string().starts_with(&prefix), "{error}");
}

// ---- probe_segment_timing ----

#[test]
fn probe_segment_timing_reports_duration_and_packets() {
    if !(on_path("ffmpeg") && on_path("ffprobe")) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = make_test_video(&dir.path().join("v.mp4"), "testsrc", 2.0);
    let info = engine().probe_segment_timing(&video).unwrap();
    assert!((info.duration_s - 2.0).abs() <= 0.2, "{info:?}");
    assert!(info.packet_count > 0);
}

// ---- parse_tenc_constant_iv_hex (pure; literal JSON shapes) ----

/// The structural shape of a real cbcs `tenc` box as mp4dump renders it (synthetic KID / IV).
fn cbcs_tenc_shape() -> Value {
    json!({
        "name": "tenc", "header_size": 12, "size": 49, "version": 1, "default_isProtected": 1,
        "default_Per_Sample_IV_Size": 0, "default_KID": bracketed(KID),
        "default_crypt_byte_block": 1, "default_skip_byte_block": 9,
        "default_constant_IV_size": 16, "default_constant_IV": bracketed(IV),
    })
}

/// A real cenc `tenc` box's shape: per-sample IVs, no constant IV field at all.
fn cenc_tenc_shape() -> Value {
    json!({
        "name": "tenc", "header_size": 12, "size": 32, "default_isProtected": 1,
        "default_Per_Sample_IV_Size": 8, "default_KID": bracketed(KID),
    })
}

fn wrap_like_mp4dump_tree(found: &Value) -> Value {
    json!([{"name": "moov", "children": [{"name": "trak", "children": [found]}]}])
}

#[test]
fn parse_tenc_constant_iv_hex_extracts_cbcs_constant_iv() {
    let tree = wrap_like_mp4dump_tree(&cbcs_tenc_shape());
    assert!(
        parse_tenc_constant_iv_hex(&tree)
            .unwrap()
            .is_some_and(|iv| iv == IV)
    );
}

#[test]
fn parse_tenc_constant_iv_hex_none_for_per_sample_iv_cenc_box() {
    let tree = wrap_like_mp4dump_tree(&cenc_tenc_shape());
    assert!(parse_tenc_constant_iv_hex(&tree).unwrap().is_none());
}

#[test]
fn parse_tenc_constant_iv_hex_none_when_no_tenc_box() {
    let tree = json!([{"name": "moov", "children": [{"name": "trak", "children": []}]}]);
    assert!(parse_tenc_constant_iv_hex(&tree).unwrap().is_none());
}

#[test]
fn a_malformed_constant_iv_is_a_value_error_that_never_quotes_it() {
    // Python's `_hex_from_bracketed_bytes` quotes the value in its ValueError; the value is the
    // IV (never included in error messages), so this implementation's message names the shape only.
    for malformed in [json!(IV), json!(format!("<{IV}>")), json!("[]")] {
        let mut shape = cbcs_tenc_shape();
        shape["default_constant_IV"] = malformed.clone();
        let tree = wrap_like_mp4dump_tree(&shape);
        for error in [
            parse_tenc_constant_iv_hex(&tree).unwrap_err(),
            parse_init_probe(&tree).unwrap_err(),
        ] {
            assert_eq!(error.python_class(), "ValueError", "{malformed}");
            assert_eq!(error.to_string(), "not a bracketed byte array");
            assert!(!format!("{error:?}").contains(IV));
        }
    }
    let mut shape = cbcs_tenc_shape();
    shape["default_constant_IV"] = json!(7);
    let error = parse_tenc_constant_iv_hex(&wrap_like_mp4dump_tree(&shape)).unwrap_err();
    assert_eq!(
        error.python_class(),
        "AttributeError",
        "Python's .strip() on an int"
    );
}

// ---- read_tenc_constant_iv_hex (real mp4dump subprocess wrapper) ----

#[test]
fn read_tenc_constant_iv_hex_real_mp4dump_no_tenc_on_unencrypted_file() {
    if !(on_path("ffmpeg") && on_path("mp4dump")) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = make_test_video(&dir.path().join("plain.mp4"), "testsrc", 1.0);
    assert!(
        engine()
            .read_tenc_constant_iv_hex(&video)
            .unwrap()
            .is_none()
    );
}

#[test]
fn real_mp4dump_output_in_the_log_carries_the_redacted_constant_iv_and_the_public_kid() {
    if !(on_path("ffmpeg") && on_path("mp4dump") && on_path("mp4encrypt")) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let clear = make_test_video(&dir.path().join("clear.mp4"), "testsrc", 1.0);
    let encrypted = dir.path().join("cbcs.mp4");
    run(&[
        "mp4encrypt",
        "--method",
        "MPEG-CBCS",
        "--key",
        &format!("1:{KEY}:{IV}"),
        "--property",
        &format!("1:KID:{KID}"),
        clear.to_str().unwrap(),
        encrypted.to_str().unwrap(),
    ]);
    let collected = Arc::new(Collector::default());
    let engine = engine_with(collected.clone(), Arc::new(Secrets::with([KEY, IV])));
    assert!(
        engine
            .read_tenc_constant_iv_hex(&encrypted)
            .unwrap()
            .is_some_and(|iv| iv == IV)
    );
    let records = collected.records();
    let text = serde_json::to_string(&records).unwrap();
    assert!(
        !text.contains(IV) && !text.contains(&bracketed(IV)) && !text.contains(&IV.to_uppercase())
    );
    let output = records
        .iter()
        .rev()
        .find(|r| r["event"] == "tool-output")
        .unwrap();
    let stdout = output["stdout"].as_str().unwrap();
    assert!(
        stdout.contains("default_constant_IV") && stdout.contains(&format!("[{REDACTED}]")),
        "{}",
        &stdout[..400.min(stdout.len())]
    );
    assert!(
        stdout.contains(&bracketed(KID)),
        "the KID is public signaling"
    );
}

#[cfg(unix)]
#[test]
fn stub_mp4dump_bracketed_iv_is_scrubbed_from_the_record() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let tree = json!([{"name": "moov", "children": [cbcs_tenc_shape()]}]);
    std::fs::write(
        dir.path().join("tree.json"),
        drmr_core::canonical::Dumps::DEFAULT.dumps(&tree),
    )
    .unwrap();
    let stub = dir.path().join("mp4dump");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\ncat '{}'\n",
            dir.path().join("tree.json").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(dir.path().join("init.mp4"), [0u8; 16]).unwrap();
    let collected = Arc::new(Collector::default());
    let mut engine = engine_with(collected.clone(), Arc::new(Secrets::with([IV])));
    engine.mp4dump_bin = stub.to_str().unwrap().to_string();
    assert!(
        engine
            .read_tenc_constant_iv_hex(&dir.path().join("init.mp4"))
            .unwrap()
            .is_some_and(|iv| iv == IV)
    );
    let text = serde_json::to_string(&collected.records()).unwrap();
    assert!(
        !text.contains(&bracketed(IV))
            && !text.contains(IV)
            && text.contains(&format!("[{REDACTED}]"))
    );
    assert!(text.contains(&bracketed(KID)), "the KID stays");
}

#[test]
fn read_tenc_constant_iv_hex_raises_on_missing_file() {
    if !on_path("mp4dump") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let error = engine()
        .read_tenc_constant_iv_hex(&dir.path().join("does-not-exist.mp4"))
        .unwrap_err();
    assert!(
        error.is_runtime_error() && matches!(error, MediaError::Runtime(_)),
        "{error:?}"
    );
}

// ---- parse_init_probe / probe_init_segment ----

fn schm(scheme: &str) -> Value {
    json!({"name": "schm", "header_size": 12, "size": 20, "scheme_type": scheme, "scheme_version": 65536})
}

fn wrap_init_like_mp4dump(boxes: &[Value]) -> Value {
    json!([{"name": "moov", "children": [{"name": "trak", "children": boxes}]}])
}

#[test]
fn parse_init_probe_cbcs_init_reports_scheme_kid_constant_iv() {
    let probe =
        parse_init_probe(&wrap_init_like_mp4dump(&[schm("cbcs"), cbcs_tenc_shape()])).unwrap();
    assert_eq!(
        probe,
        InitProbe {
            scheme_type: Some("cbcs".into()),
            constant_iv_hex: Some(HexSecret::new(IV.into())),
            default_kid_hex: Some(KID.into()),
            per_sample_iv_size: Some(0),
        }
    );
}

#[test]
fn parse_init_probe_cenc_init_reports_scheme_kid_no_constant_iv() {
    let probe =
        parse_init_probe(&wrap_init_like_mp4dump(&[schm("cenc"), cenc_tenc_shape()])).unwrap();
    assert_eq!(probe.scheme_type.as_deref(), Some("cenc"));
    assert!(probe.constant_iv_hex.is_none());
    assert_eq!(probe.default_kid_hex.as_deref(), Some(KID));
    assert_eq!(probe.per_sample_iv_size, Some(8));
}

#[test]
fn parse_init_probe_all_none_for_unencrypted_init() {
    assert_eq!(
        parse_init_probe(&wrap_init_like_mp4dump(&[])).unwrap(),
        InitProbe::default()
    );
}

#[test]
fn probe_init_segment_real_mp4dump_all_none_on_unencrypted_file() {
    if !(on_path("ffmpeg") && on_path("mp4dump")) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = make_test_video(&dir.path().join("plain.mp4"), "testsrc", 1.0);
    assert_eq!(
        engine().probe_init_segment(&video).unwrap(),
        InitProbe::default()
    );
}

#[test]
fn probe_init_segment_raises_on_missing_file() {
    if !on_path("mp4dump") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let error = engine()
        .probe_init_segment(&dir.path().join("does-not-exist.mp4"))
        .unwrap_err();
    assert!(matches!(error, MediaError::Runtime(_)), "{error:?}");
}

// ---- parse_fragment_boundary (pure; literal JSON shape) ----

fn wrap_fragment_like_mp4dump(tfdt: u64, trun_sample_count: u64, include_saiz: bool) -> Value {
    let mut traf = vec![
        json!({"name": "tfhd", "header_size": 12, "size": 28}),
        json!({"name": "tfdt", "header_size": 12, "size": 16, "base media decode time": tfdt}),
        json!({"name": "trun", "header_size": 12, "size": 1144, "sample count": trun_sample_count}),
    ];
    if include_saiz {
        // A different "sample count" field the parser must not bind to.
        traf.push(json!({"name": "saiz", "sample count": trun_sample_count + 999}));
    }
    json!([{"name": "styp"}, {"name": "moof", "children": [{"name": "mfhd"}, {"name": "traf", "children": traf}]}, {"name": "mdat"}])
}

#[test]
fn parse_fragment_boundary_extracts_tfdt_and_trun_sample_count() {
    let boundary =
        parse_fragment_boundary(&wrap_fragment_like_mp4dump(287_744, 281, true)).unwrap();
    assert_eq!(
        (boundary.base_media_decode_time, boundary.sample_count),
        (json!(287_744), json!(281))
    );
}

#[test]
fn parse_fragment_boundary_binds_to_trun_not_saiz_sample_count() {
    let boundary = parse_fragment_boundary(&wrap_fragment_like_mp4dump(0, 281, true)).unwrap();
    assert_eq!(boundary.sample_count, json!(281));
}

#[test]
fn parse_fragment_boundary_none_when_no_moof() {
    assert!(
        parse_fragment_boundary(&json!([{"name": "ftyp"}, {"name": "moov", "children": []}]))
            .is_none()
    );
}

// ---- read_fragment_boundary (real mp4dump subprocess wrapper) ----

#[test]
fn read_fragment_boundary_raises_on_missing_file() {
    if !on_path("mp4dump") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let error = engine()
        .read_fragment_boundary(&dir.path().join("does-not-exist.m4s"))
        .unwrap_err();
    assert!(matches!(error, MediaError::Runtime(_)), "{error:?}");
}

#[test]
fn read_fragment_boundary_none_on_non_fragmented_file() {
    if !(on_path("ffmpeg") && on_path("mp4dump")) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let video = make_test_video(&dir.path().join("plain.mp4"), "testsrc", 1.0);
    assert!(engine().read_fragment_boundary(&video).unwrap().is_none());
}

// ---- extract_hls_iv_hex_values ----

#[test]
fn extract_hls_iv_hex_values_matches_real_playlist_line() {
    let synthetic_iv = IV.to_uppercase();
    let playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://sample-content-id\",IV=0x{synthetic_iv},KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXTINF:6,\nseg_0.m4s\n"
    );
    assert_eq!(
        extract_hls_iv_hex_values(&playlist),
        vec![HexSecret::new(IV.into())]
    );
}

#[test]
fn extract_hls_iv_hex_values_multiple_lines() {
    let iv = "ab".repeat(16);
    let playlist: Vec<String> = (0..3)
        .map(|_| format!("#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\",IV=0x{iv},KEYFORMAT=\"y\""))
        .collect();
    assert_eq!(
        extract_hls_iv_hex_values(&playlist.join("\n")),
        vec![HexSecret::new(iv); 3]
    );
}

#[test]
fn extract_hls_iv_hex_values_empty_when_absent() {
    assert_eq!(
        extract_hls_iv_hex_values("#EXTM3U\n#EXTINF:6,\nseg_0.m4s\n"),
        Vec::<HexSecret>::new()
    );
}

// ---- localize_hls_media_playlist (pure; the SAMPLE-AES decrypt-localizer) ----

const SYNTH_TS_AUDIO_PLAYLIST: &str = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://0011223344556677\",IV=0x20202020202020202020202020202020,KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXTINF:6,\nhttps://vod.test/t/s/audio/uuid/seg_0.ts\n#EXTINF:6,\nhttps://vod.test/t/s/audio/uuid/seg_1.ts\n#EXT-X-ENDLIST\n";
const SYNTH_FMP4_PLAYLIST: &str = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-MAP:URI=\"https://vod.test/t/s/video/uuid/init.mp4\"\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://abcdef\",IV=0x20202020202020202020202020202020,KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXTINF:6,\nhttps://vod.test/t/s/video/uuid/seg_0.m4s\n#EXT-X-ENDLIST\n";

/// `_fake_resolver`: any URL to a deterministic local path (nothing read).
fn fake_resolver(url: &str) -> Result<PathBuf, MediaError> {
    local_path(url).ok_or_else(|| MediaError::Value(format!("not a URL: {url}")))
}

fn local_path(url: &str) -> Option<PathBuf> {
    url.split_once("://")
        .map(|(_, rest)| Path::new("/local").join(rest))
}

fn segment_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

fn key_file(dir: &Path) -> PathBuf {
    let path = dir.join("content.key");
    std::fs::write(&path, [0u8; 16]).unwrap();
    path
}

#[test]
fn localize_rewrites_key_to_identity_and_local_file() {
    let dir = tempfile::tempdir().unwrap();
    let key = key_file(dir.path());
    let out = localize_hls_media_playlist(SYNTH_TS_AUDIO_PLAYLIST, &key, fake_resolver).unwrap();
    assert!(out.contains("KEYFORMAT=\"identity\""));
    assert!(!out.contains("KEYFORMAT=\"com.apple.streamingkeydelivery\""));
    let resolved = python::path_str(&python::resolve(&key).unwrap()).unwrap();
    assert!(out.contains(&format!("URI=\"{resolved}\"")), "{out}");
    assert!(!out.contains("skd://"));
    assert!(
        out.contains("IV=0x20202020202020202020202020202020"),
        "the IV is left for ffmpeg"
    );
}

#[test]
fn localize_rewrites_segments_to_local_paths() {
    let dir = tempfile::tempdir().unwrap();
    let out = localize_hls_media_playlist(
        SYNTH_TS_AUDIO_PLAYLIST,
        &key_file(dir.path()),
        fake_resolver,
    )
    .unwrap();
    assert_eq!(
        segment_lines(&out),
        [
            "/local/vod.test/t/s/audio/uuid/seg_0.ts",
            "/local/vod.test/t/s/audio/uuid/seg_1.ts"
        ]
    );
    assert!(out.contains("#EXT-X-TARGETDURATION:6") && out.contains("#EXT-X-ENDLIST"));
}

#[test]
fn localize_rewrites_ext_x_map_uri() {
    let dir = tempfile::tempdir().unwrap();
    let out =
        localize_hls_media_playlist(SYNTH_FMP4_PLAYLIST, &key_file(dir.path()), fake_resolver)
            .unwrap();
    assert!(out.contains("#EXT-X-MAP:URI=\"/local/vod.test/t/s/video/uuid/init.mp4\""));
    assert_eq!(
        segment_lines(&out),
        ["/local/vod.test/t/s/video/uuid/seg_0.m4s"]
    );
}

#[test]
fn write_raw_key_file_writes_16_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let dest = write_raw_key_file(
        "00112233445566778899aabbccddeeff",
        &dir.path().join("k").join("content.key"),
    )
    .unwrap();
    assert_eq!(
        std::fs::read(&dest).unwrap(),
        hex::decode("00112233445566778899aabbccddeeff").unwrap()
    );
    assert_eq!(std::fs::read(&dest).unwrap().len(), 16);
}

// ---- localize_hls_media_playlist_http / localize_hls_master_http ----

fn fake_http_resolver(url: &str) -> Result<String, MediaError> {
    url.split_once("://")
        .map(|(_, rest)| format!("/media/{rest}"))
        .ok_or_else(|| MediaError::Value(format!("not a URL: {url}")))
}

#[test]
fn localize_http_rewrites_key_to_identity_and_key_route() {
    let out = localize_hls_media_playlist_http(
        SYNTH_TS_AUDIO_PLAYLIST,
        "/fps/key.bin",
        fake_http_resolver,
    )
    .unwrap();
    assert!(
        out.contains("KEYFORMAT=\"identity\"")
            && !out.contains("KEYFORMAT=\"com.apple.streamingkeydelivery\"")
    );
    assert!(out.contains("URI=\"/fps/key.bin\"") && !out.contains("skd://"));
    assert!(
        out.contains("IV=0x20202020202020202020202020202020"),
        "IV untouched, ffmpeg-localizer parity"
    );
}

#[test]
fn localize_http_rewrites_segments_to_media_routes() {
    let out = localize_hls_media_playlist_http(
        SYNTH_TS_AUDIO_PLAYLIST,
        "/fps/key.bin",
        fake_http_resolver,
    )
    .unwrap();
    assert_eq!(
        segment_lines(&out),
        [
            "/media/vod.test/t/s/audio/uuid/seg_0.ts",
            "/media/vod.test/t/s/audio/uuid/seg_1.ts"
        ]
    );
}

#[test]
fn localize_http_rewrites_ext_x_map_uri() {
    let out =
        localize_hls_media_playlist_http(SYNTH_FMP4_PLAYLIST, "/fps/key.bin", fake_http_resolver)
            .unwrap();
    assert!(out.contains("#EXT-X-MAP:URI=\"/media/vod.test/t/s/video/uuid/init.mp4\""));
}

const SYNTH_MASTER: &str = "#EXTM3U\n#EXT-X-VERSION:4\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"fmp4_AUDIO-0\",NAME=\"audio\",URI=\"https://variants.test/audio/u0/pl.m3u8?vt=tok\"\n#EXT-X-STREAM-INF:BANDWIDTH=600000,CODECS=\"avc1.42C00C,mp4a.40.2\",AUDIO=\"fmp4_AUDIO-0\"\nhttps://variants.test/video/u1/pl.m3u8?vt=tok\n#EXT-X-STREAM-INF:BANDWIDTH=900000,CODECS=\"hvc1.1.6.L60.90,mp4a.40.2\",AUDIO=\"fmp4_AUDIO-0\"\nhttps://variants.test/video/u2/pl.m3u8?vt=tok\n";

/// Simulates `GET /fps/playlist?path=<original>` (an empty URL is no variant).
fn fake_playlist_route(url: &str) -> Result<String, MediaError> {
    if url.is_empty() {
        return Err(MediaError::Value("no URL".to_string()));
    }
    Ok(format!("/fps/playlist?path={url}"))
}

#[test]
fn localize_master_http_rewrites_variant_uris() {
    let out = localize_hls_master_http(SYNTH_MASTER, fake_playlist_route).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines[4],
        "/fps/playlist?path=https://variants.test/video/u1/pl.m3u8?vt=tok"
    );
    assert_eq!(
        lines[6],
        "/fps/playlist?path=https://variants.test/video/u2/pl.m3u8?vt=tok"
    );
}

#[test]
fn localize_master_http_rewrites_ext_x_media_uri() {
    let out = localize_hls_master_http(SYNTH_MASTER, fake_playlist_route).unwrap();
    assert!(
        out.contains("URI=\"/fps/playlist?path=https://variants.test/audio/u0/pl.m3u8?vt=tok\"")
    );
}

#[test]
fn localize_master_http_preserves_untouched_lines() {
    let out = localize_hls_master_http(SYNTH_MASTER, fake_playlist_route).unwrap();
    assert!(out.contains("#EXT-X-VERSION:4"));
    assert!(
        out.contains("BANDWIDTH=600000,CODECS=\"avc1.42C00C,mp4a.40.2\",AUDIO=\"fmp4_AUDIO-0\"")
    );
}

// ---- beyond the Python cases ----

fn pure_fixture() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/pure.python311.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn the_localizers_expand_their_uris_as_pythons_re_sub_does() {
    // re.sub interprets backslash escapes in the replacement (a `\n` becomes a line break, `\1`
    // is an error) and never `$`; a segment URI line is replaced as is. CPython 3.11's output.
    let fixture = pure_fixture();
    let playlist = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\",IV=0x20202020202020202020202020202020,KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6,\nseg 0.m4s\n  indented\n#EXT-X-ENDLIST\n";
    for case in fixture["key_uri_templates"].as_array().unwrap() {
        let key_uri = case[0].as_str().unwrap();
        let result = localize_hls_media_playlist_http(playlist, key_uri, |url| {
            Ok::<_, MediaError>(format!("/media/{url}"))
        });
        match case[1].as_str().unwrap() {
            "ok" => assert_eq!(result.unwrap(), case[2].as_str().unwrap(), "{key_uri:?}"),
            class => {
                let error = result.unwrap_err();
                let expected_class = if class == "error" { "re.error" } else { class };
                assert_eq!(error.python_class(), expected_class, "{key_uri:?}");
                assert_eq!(error.to_string(), case[2].as_str().unwrap(), "{key_uri:?}");
            }
        }
    }
    let master = &fixture["master"];
    let out = localize_hls_master_http(master[0].as_str().unwrap(), |url| {
        Ok::<_, MediaError>(format!("/p?u={url}$1\\n"))
    })
    .unwrap();
    assert_eq!(out, master[1].as_str().unwrap());
}

#[test]
fn a_resolver_error_comes_back_unchanged() {
    #[derive(Debug, PartialEq)]
    struct Escapes;
    impl From<MediaError> for Escapes {
        fn from(_: MediaError) -> Self {
            Escapes
        }
    }
    let result = localize_hls_media_playlist_http(SYNTH_TS_AUDIO_PLAYLIST, "/k", |_| {
        Err::<String, _>(Escapes)
    });
    assert_eq!(result, Err(Escapes));
}

#[cfg(unix)]
#[test]
fn the_raw_key_file_is_private_and_no_error_carries_the_key() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let fresh = write_raw_key_file(KEY, &dir.path().join("k/content.key")).unwrap();
    assert_eq!(
        std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let existing = dir.path().join("existing.key");
    std::fs::write(&existing, b"old contents, longer than a key").unwrap();
    std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_raw_key_file(KEY, &existing).unwrap();
    assert_eq!(
        std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::read(&existing).unwrap(),
        hex::decode(KEY).unwrap(),
        "truncated and rewritten"
    );
    let bad = format!("{}zz", &KEY[..30]);
    let error = write_raw_key_file(&bad, &dir.path().join("bad.key")).unwrap_err();
    assert_eq!(
        (error.python_class(), error.to_string()),
        (
            "ValueError",
            "non-hexadecimal number found in fromhex() arg at position 30".to_string()
        )
    );
    assert!(!format!("{error:?}").contains(&KEY[..30]));
    assert!(
        !dir.path().join("bad.key").exists(),
        "nothing written before the key decodes"
    );
}

/// The init and segments of a synthetic fragmented video, split with the Python rules.
fn fragments(dir: &Path) -> (PathBuf, Vec<PathBuf>) {
    let source = dir.join("av.mp4");
    run(&[
        "ffmpeg",
        "-v",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc=duration=3:size=64x64:rate=5",
        "-c:v",
        "libx264",
        "-g",
        "5",
        "-movflags",
        "+frag_keyframe+empty_moov+default_base_moof",
        source.to_str().unwrap(),
    ]);
    let init = dir.join("init.mp4");
    let media = drmr_core::media::split_fragmented_mp4(&source, &init).unwrap();
    let segments = media
        .iter()
        .enumerate()
        .map(|(index, bytes)| {
            let path = dir.join(format!("seg_{index}.m4s"));
            std::fs::write(&path, bytes).unwrap();
            path
        })
        .collect();
    (init, segments)
}

#[test]
fn segment_durations_are_tfdt_steps_and_the_last_ends_at_ffprobes_duration() {
    if !(on_path("ffmpeg") && on_path("ffprobe") && on_path("mp4dump")) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (init, segments) = fragments(dir.path());
    let engine = engine();
    let durations = engine
        .measure_segment_durations_s(&init, &segments, 10_240)
        .unwrap();
    assert_eq!(durations.len(), segments.len());
    assert!(
        durations[..durations.len() - 1]
            .iter()
            .all(|d| (*d - 1.0).abs() < 1e-12),
        "{durations:?}"
    );
    assert!(
        (durations.last().unwrap() - 1.0).abs() < 0.05,
        "{durations:?}"
    );
    assert_eq!(
        engine.measure_durations(&init, &[], 10_240).unwrap(),
        Vec::<f64>::new()
    );
    let error = engine
        .measure_segment_durations_s(&init, std::slice::from_ref(&init), 10_240)
        .unwrap_err();
    assert!(error.is_runtime_error());
    assert_eq!(
        error.to_string(),
        format!(
            "{} has no fragment (moof/tfdt/trun) metadata to measure",
            python::path_str(&init).unwrap()
        )
    );
}

/// An HLS rendition of `clear` (AAC in TS, 2 s segments), AES-128 encrypted when `keyinfo`
/// is given; returns its playlist text.
fn hls_of(clear: &Path, playlist: &Path, keyinfo: Option<&Path>) -> String {
    let mut argv = vec![
        "ffmpeg",
        "-v",
        "error",
        "-y",
        "-i",
        clear.to_str().unwrap(),
        "-c",
        "copy",
    ];
    argv.extend(["-f", "hls", "-hls_time", "2", "-hls_playlist_type", "vod"]);
    if let Some(info) = keyinfo {
        argv.extend(["-hls_key_info_file", info.to_str().unwrap()]);
    }
    argv.push(playlist.to_str().unwrap());
    run(&argv);
    std::fs::read_to_string(playlist).unwrap()
}

#[test]
fn an_identity_key_playlist_decodes_like_the_same_rendition_in_the_clear() {
    if !on_path("ffmpeg") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let clear = root.join("clear.mp4");
    run(&[
        "ffmpeg",
        "-v",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000",
        "-t",
        "4",
        "-c:a",
        "aac",
        clear.to_str().unwrap(),
    ]);
    // AES-128 with the synthetic key; the playlist's key URI is one ffmpeg must never fetch.
    let key = write_raw_key_file(KEY, &root.join("work/content.key")).unwrap();
    std::fs::write(
        root.join("keyinfo"),
        format!("skd://sample-content-id\n{}\n{IV}\n", key.display()),
    )
    .unwrap();
    let encrypted = hls_of(&clear, &root.join("enc.m3u8"), Some(&root.join("keyinfo")));
    let plain = hls_of(&clear, &root.join("plain.m3u8"), None);
    assert!(encrypted.contains("skd://sample-content-id") && !plain.contains("#EXT-X-KEY"));
    let local = |url: &str| Ok::<_, MediaError>(root.join(url));
    let collected = Arc::new(Collector::default());
    let engine = engine_with(collected.clone(), Arc::new(Secrets::with([KEY, IV])));
    let decode = |text: &str, key_file: &Path, name: &str| {
        let localized = root.join(format!("work/{name}.m3u8"));
        std::fs::write(
            &localized,
            localize_hls_media_playlist(text, key_file, local).unwrap(),
        )
        .unwrap();
        engine
            .decode_hls_media_playlist(&localized, &root.join(format!("work/{name}.pcm")), "pcm")
            .map(|out| std::fs::read(out).unwrap())
    };
    let decrypted = decode(&encrypted, &key, "decrypted").unwrap();
    let reference = decode(&plain, &key, "reference").unwrap();
    assert!(
        !decrypted.is_empty() && decrypted == reference,
        "{} vs {} bytes",
        decrypted.len(),
        reference.len()
    );
    let wrong = write_raw_key_file(&"01".repeat(16), &root.join("work/wrong.key")).unwrap();
    assert!(
        decode(&encrypted, &wrong, "wrong").map_or(true, |pcm| pcm != reference),
        "a wrong key never decodes the same"
    );
    // The framemd5 mode drops audio (-an): an audio-only rendition is ffmpeg's failure, reimplemented
    // as Python reports it (the playlist's name, never the key file's contents).
    let localized = root.join("work/decrypted.m3u8");
    let error = engine
        .decode_hls_media_playlist(
            &localized,
            &root.join("work/decrypted.framemd5"),
            "framemd5",
        )
        .unwrap_err();
    assert!(error.is_runtime_error());
    assert_eq!(
        error.to_string(),
        "ffmpeg SAMPLE-AES decode (framemd5) failed for decrypted.m3u8"
    );
    let log = serde_json::to_string(&collected.records()).unwrap();
    assert!(
        !log.contains(KEY) && !log.contains(IV),
        "no key material in any record"
    );
}
