//! Clear-HLS packaging through injected dependencies, including failure handling.

#[path = "common/mod.rs"]
mod private_fixture;

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier, Mutex};
use std::thread::ThreadId;

use drmr_core::concurrency::Pool;
use drmr_core::config::KeyMaterial;
use drmr_core::events::Record;
use drmr_core::hls_pipeline::{
    DrmSignaling, PackagingSelection, PipelineDeps, PipelineError, TrackError, TrackJob, copy_text,
    frac, package_hls, package_track, protobuf_kids, selected_tracks, to_fraction,
    validate_signaling,
};
use drmr_core::manifest_gen::{PackagedPresentation, PrivateFairPlaySignaling};
use drmr_core::manifest_support::py::PyErr;
use drmr_core::media::{HexSecret, InitProbe, MediaError, PacketInfo};
use drmr_core::packager::{self, PackagerResult};
use drmr_core::pipeline_deps::EncryptCbcs;
use drmr_core::playready::{PLAYREADY_SYSTEM_ID, WIDEVINE_SYSTEM_ID, build_pssh_box};
use drmr_core::source::{Failure, SourceAnalysis, SourceError, SourceSegment, SourceTrack};
use num_rational::BigRational;
use serde_json::{Value, json};

const KID: &str = "000102030405060708090a0b0c0d0e0f";

fn keys() -> KeyMaterial {
    KeyMaterial::new(KID, "10".repeat(16), "20".repeat(16)).expect("synthetic keys")
}

fn signaling_with(pssh: Vec<u8>) -> DrmSignaling {
    DrmSignaling {
        widevine_pssh: pssh,
        playready_la_url: "https://license.example/pr".to_string(),
        fairplay_uri: "skd://asset".to_string(),
    }
}

fn widevine_payload(kid_hex: &str) -> Vec<u8> {
    let mut payload = vec![0x12, 0x10];
    payload.extend(hex::decode(kid_hex).expect("hex"));
    payload
}

fn widevine_box(payload: &[u8]) -> Vec<u8> {
    build_pssh_box(&WIDEVINE_SYSTEM_ID, payload).expect("a small box")
}

fn signaling() -> DrmSignaling {
    signaling_with(widevine_box(&widevine_payload(KID)))
}

/// `SourceSegment(index, path, duration, sha256)` with the dataclass defaults.
fn segment(index: i64, path: impl Into<PathBuf>, duration: f64) -> SourceSegment {
    SourceSegment {
        index,
        path: path.into(),
        duration,
        sha256: "unused".to_string(),
        byte_range: None,
    }
}

/// `SourceTrack(id, content_type, container, codec, segments)` with the dataclass defaults.
fn track(
    id: &str,
    content_type: &str,
    container: &str,
    codec: &str,
    segments: Vec<SourceSegment>,
) -> SourceTrack {
    SourceTrack {
        id: id.to_string(),
        content_type: content_type.to_string(),
        container: container.to_string(),
        codec: codec.to_string(),
        segments,
        init_path: None,
        init_sha256: None,
        language: "und".to_string(),
        role: "main".to_string(),
        group_id: String::new(),
        name: String::new(),
        width: None,
        height: None,
        bandwidth: None,
        codecs: String::new(),
        stream_index: 0,
        playlist_id: String::new(),
        muxed: false,
        audio_group: String::new(),
        subtitle_group: String::new(),
        profile: String::new(),
        is_default: false,
        autoselect: false,
    }
}

/// `int(Path(path).stem.rsplit('_', 1)[1])` — the segment number a work file is named after.
fn number_of(path: &Path) -> usize {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    stem.rsplit_once('_')
        .and_then(|(_, n)| n.parse().ok())
        .unwrap_or(0)
}

fn result(returncode: i32) -> PackagerResult {
    PackagerResult {
        argv_redacted: vec![],
        returncode,
        duration_s: 0.0,
        stderr_tail: String::new(),
    }
}

#[derive(Default)]
struct Fake {
    events: Mutex<Vec<(String, Option<usize>)>>,
    threads: Mutex<HashSet<ThreadId>>,
    /// The prepare (segment number) whose encryption fails.
    failing: Option<usize>,
    /// Waited on by the first concatenate of every track (`source_0`).
    barrier: Option<Arc<Barrier>>,
    /// The track whose decoded identity check fails.
    mismatch: Option<String>,
    payloads: Vec<Vec<u8>>,
    snapshot_error: Option<String>,
    /// mp4dump fails on every init (a `RuntimeError`, which `convert` does not catch).
    init_probe_fails: bool,
    /// What `write_dash` raises.
    dash_error: Option<PyErr>,
}

impl Fake {
    fn note(&self, kind: &str, number: Option<usize>) {
        self.events.lock().unwrap().push((kind.to_string(), number));
    }

    fn write(path: &Path, payload: &[u8]) -> Result<PathBuf, MediaError> {
        use std::io::Write as _;
        if let Some(parent) = path.parent() {
            drmr_core::native::create_private_dir_all(parent)?;
        }
        drmr_core::native::create_private_or_truncate(path)?.write_all(payload)?;
        Ok(path.to_path_buf())
    }

    fn events(&self) -> Vec<(String, Option<usize>)> {
        self.events.lock().unwrap().clone()
    }
}

impl PipelineDeps for Fake {
    fn concatenate(&self, _parts: &[PathBuf], output: &Path) -> Result<PathBuf, MediaError> {
        self.threads
            .lock()
            .unwrap()
            .insert(std::thread::current().id());
        let first_of_track = output
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("source_"));
        if let Some(barrier) = self.barrier.as_ref().filter(|_| first_of_track) {
            barrier.wait();
        }
        Fake::write(output, b"x")
    }

    fn probe_packets(&self, path: &Path, _stream_index: i64) -> Result<PacketInfo, MediaError> {
        // one 6 s segment per number, back to back on a 90 kHz timeline
        let n = i64::try_from(number_of(path)).expect("small");
        Ok(PacketInfo {
            timescale: 90000,
            dts: vec![n * 540_000],
            pts: vec![n * 540_000],
            durations: vec![540_000],
            frame_rate: "24/1".to_string(),
        })
    }

    fn packet_payloads(
        &self,
        _path: &Path,
        _stream_index: i64,
    ) -> Result<Vec<Vec<u8>>, MediaError> {
        Ok(self.payloads.clone())
    }

    fn packet_payload_hashes(
        &self,
        _path: &Path,
        _stream_index: i64,
    ) -> Result<Vec<Value>, MediaError> {
        Ok(vec![json!("SHA256:same")])
    }

    fn normalize_segment(
        &self,
        _p: &Path,
        destination: &Path,
        _s: i64,
        _o: &BigRational,
        _c: &str,
    ) -> Result<PathBuf, MediaError> {
        Fake::write(destination, b"n")
    }

    fn same_timing(
        &self,
        _s: &PacketInfo,
        _o: &PacketInfo,
        _shift: &BigRational,
    ) -> Result<bool, MediaError> {
        Ok(true)
    }

    fn decoded_digest(&self, path: &Path, _c: &str, _s: i64) -> Result<String, MediaError> {
        let Some(track) = self.mismatch.as_deref() else {
            return Ok("same".to_string());
        };
        let of_track = path.components().any(|part| part.as_os_str() == track)
            || path.file_stem().is_some_and(|stem| stem == track);
        let differing = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("decrypted_"))
            || path
                .components()
                .any(|part| part.as_os_str() == "audio-equivalence");
        Ok(if of_track && differing {
            "other".to_string()
        } else {
            "same".to_string()
        })
    }

    fn codec_from_init(&self, _p: &Path, _f: &str) -> Result<String, MediaError> {
        Ok("avc1.64001e".to_string())
    }

    fn split_fragmented_mp4(&self, _s: &Path, _i: &Path) -> Result<Vec<Vec<u8>>, MediaError> {
        Ok(Vec::new())
    }

    fn box_tree(&self, path: &Path) -> Result<Value, MediaError> {
        let audio = path.components().any(|part| {
            part.as_os_str()
                .to_str()
                .is_some_and(|name| name.starts_with("a0"))
        });
        let (crypt, skip) = if audio { (0, 0) } else { (1, 9) };
        Ok(json!([{"name": "moov", "children": [
            {"name": "tenc", "default_crypt_byte_block": crypt, "default_skip_byte_block": skip}
        ]}]))
    }

    fn probe_init_segment(&self, _p: &Path) -> Result<InitProbe, MediaError> {
        self.note("validate-init", None);
        if self.init_probe_fails {
            return Err(MediaError::Runtime(
                "mp4dump failed for <init>: x".to_string(),
            ));
        }
        Ok(InitProbe {
            scheme_type: Some("cbcs".to_string()),
            constant_iv_hex: Some(HexSecret::new("20".repeat(16))),
            default_kid_hex: Some(KID.to_string()),
            per_sample_iv_size: Some(0),
        })
    }

    fn pssh_box_present(&self, _p: &Path, _b: &[u8]) -> Result<bool, MediaError> {
        Ok(true)
    }

    fn encrypt_cbcs(&self, call: &EncryptCbcs<'_>) -> Result<PackagerResult, packager::Error> {
        let n = number_of(call.input_mp4);
        self.note("prepare", Some(n));
        if self.failing == Some(n) {
            return Ok(result(1));
        }
        Fake::write(call.init_output, b"init")
            .map_err(|e| packager::Error::Value(e.to_string()))?;
        Fake::write(
            Path::new(&call.segment_template.replace("$Number$", "1")),
            b"seg",
        )
        .map_err(|e| packager::Error::Value(e.to_string()))?;
        Ok(result(0))
    }

    fn decrypt_cenc(
        &self,
        encrypted: &Path,
        clear: &Path,
        _k: &KeyMaterial,
        _b: &str,
    ) -> Result<PackagerResult, packager::Error> {
        self.note("finish", Some(number_of(encrypted)));
        Fake::write(clear, b"clear").map_err(|e| packager::Error::Value(e.to_string()))?;
        Ok(result(0))
    }

    fn verify_source_snapshot(&self, _a: &SourceAnalysis) -> Result<(), Failure> {
        match &self.snapshot_error {
            Some(token) => Err(Failure::Source(SourceError::new(token))),
            None => Ok(()),
        }
    }

    fn write_dash(&self, _p: &PackagedPresentation, path: &Path) -> Result<PathBuf, PyErr> {
        if let Some(error) = &self.dash_error {
            return Err(error.clone());
        }
        Fake::write(path, b"<MPD/>").map_err(|e| PyErr::Value(e.to_string()))
    }

    fn write_hls(
        &self,
        _p: &PackagedPresentation,
        path: &Path,
        _f: &PrivateFairPlaySignaling,
    ) -> Result<Vec<PathBuf>, PyErr> {
        Fake::write(path, b"#EXTM3U\n").map_err(|e| PyErr::Value(e.to_string()))?;
        Ok(vec![path.to_path_buf()])
    }
}

fn analysis(tmp: &Path, tracks: Vec<SourceTrack>) -> SourceAnalysis {
    let snapshot = tmp.join("asset/clear/source/analysis.json");
    drmr_core::native::create_private_dir_all(snapshot.parent().unwrap()).unwrap();
    SourceAnalysis {
        schema_version: "1".to_string(),
        source_kind: "hls-clear".to_string(),
        analysis_id: "hls-test".to_string(),
        asset_id: "asset".to_string(),
        admitted: true,
        tracks,
        findings: vec![],
        snapshot_path: snapshot,
        snapshot_digest: "digest".to_string(),
    }
}

#[test]
fn three_drm_signaling_uses_supplied_widevine_and_v43_playready() {
    let pr = validate_signaling(&signaling(), &keys()).expect("valid");
    assert!(
        pr.wrmheader_xml.contains(r#"version="4.3.0.0""#)
            && pr.wrmheader_xml.contains("AESCBC")
            && !pr.wrmheader_xml.contains("CHECKSUM")
    );
    // the PlayReady generator: the LA_URL as given, one PlayReady pssh box
    assert!(
        pr.wrmheader_xml
            .contains("<LA_URL>https://license.example/pr</LA_URL>")
    );
    assert_eq!(&pr.pssh_box[12..28], &PLAYREADY_SYSTEM_ID);
    assert_eq!(pr.pssh_hex, hex::encode(&pr.pssh_box));
}

#[test]
fn rejects_invalid_wrong_system_or_mismatched_widevine() {
    let cases = [
        (b"bad".to_vec(), "widevine-invalid-pssh"),
        (
            widevine_box(&widevine_payload(&"ff".repeat(16))),
            "widevine-kid-mismatch",
        ),
        (
            build_pssh_box(&[0x11; 16], b"").unwrap(),
            "widevine-invalid-pssh",
        ),
    ];
    for (pssh, token) in cases {
        assert_eq!(
            validate_signaling(&signaling_with(pssh), &keys()).unwrap_err(),
            PipelineError::Value(token.to_string())
        );
    }
    // a version-1 box with a KID list naming the content KID, and a payload without KIDs
    let mut v1 = vec![0, 0, 0, 0, b'p', b's', b's', b'h', 1, 0, 0, 0];
    v1.extend_from_slice(&WIDEVINE_SYSTEM_ID);
    v1.extend_from_slice(&[0, 0, 0, 1]);
    v1.extend(hex::decode(KID).unwrap());
    v1.extend_from_slice(&[0, 0, 0, 0]);
    let size = u32::try_from(v1.len()).unwrap().to_be_bytes();
    v1[..4].copy_from_slice(&size);
    assert!(validate_signaling(&signaling_with(v1), &keys()).is_ok());
}

#[test]
fn rejects_bad_fairplay_and_playready_uris() {
    for uri in [
        "https://example.invalid/key",
        "skd://x\"\n#EXT-X-ENDLIST",
        "skd://",
        "skd://user@asset",
    ] {
        let mut sig = signaling();
        sig.fairplay_uri = uri.to_string();
        assert_eq!(
            validate_signaling(&sig, &keys()).unwrap_err(),
            PipelineError::Value("fairplay-invalid-uri".to_string()),
            "{uri:?}"
        );
    }
    for la_url in [
        "skd://license",
        "https://",
        "https://u:p@license.example/pr",
        "ftp://license.example/pr",
    ] {
        let mut sig = signaling();
        sig.playready_la_url = la_url.to_string();
        assert_eq!(
            validate_signaling(&sig, &keys()).unwrap_err(),
            PipelineError::Value("playready-invalid-uri".to_string()),
            "{la_url:?}"
        );
    }
    // explicit HTTP test/provider URLs are preserved as well as HTTPS license endpoints
    let mut sig = signaling();
    sig.playready_la_url = "http://pr.test.drm.com/playready/RightsManager.asmx".to_string();
    assert!(validate_signaling(&sig, &keys()).is_ok());
}

#[test]
fn uris_are_split_as_cpython_splits_them() {
    // CPython 3.11's urlsplit refuses a netloc that NFKC turns into a delimiter, and a
    // bracketed host that is not the whole host; the implementation's own splitter once accepted these.
    for uri in ["skd://a\u{ff0f}b", "skd://x[::1]", "skd://[::1]x"] {
        let mut sig = signaling();
        sig.fairplay_uri = uri.to_string();
        assert_eq!(
            validate_signaling(&sig, &keys()).unwrap_err(),
            PipelineError::Value("fairplay-invalid-uri".to_string()),
            "{uri:?}"
        );
    }
    let mut sig = signaling();
    sig.fairplay_uri = "skd://[::1]:443/asset".to_string();
    assert!(validate_signaling(&sig, &keys()).is_ok());
}

#[test]
fn the_kid_is_read_with_bytes_fromhex() {
    // `bytes.fromhex` takes whitespace between the pairs, as `KeyMaterial` did: the Widevine
    // KID check passes, and the KID is refused where Python refuses it — by `uuid.UUID` in
    // `build_playready_v43`, with its `PlayReadyError` (the reference fixture's exact message).
    let spaced = KID
        .as_bytes()
        .chunks(2)
        .map(|pair| std::str::from_utf8(pair).unwrap())
        .collect::<Vec<_>>()
        .join(" ");
    let keys = KeyMaterial::new(spaced.clone(), "10".repeat(16), "20".repeat(16)).unwrap();
    assert_eq!(
        validate_signaling(&signaling(), &keys).unwrap_err(),
        PipelineError::Value(format!("not a valid KID/UUID: '{spaced}'"))
    );
    // a foreign KID is still the Widevine mismatch, read the same way
    let foreign = KeyMaterial::new("ff ".repeat(16), "10".repeat(16), "20".repeat(16)).unwrap();
    assert_eq!(
        validate_signaling(&signaling(), &foreign).unwrap_err(),
        PipelineError::Value("widevine-kid-mismatch".to_string())
    );
}

#[test]
fn signaling_debug_contains_no_provider_values() {
    let text = format!("{:?}", signaling());
    assert!(
        !text.contains("license.example") && !text.contains("skd://"),
        "{text}"
    );
}

#[test]
fn protobuf_kids_reads_field_two_and_rejects_malformed_payloads() {
    let mut payload = vec![0x08, 0x01];
    payload.extend(widevine_payload(KID));
    assert_eq!(
        protobuf_kids(&payload).unwrap(),
        vec![hex::decode(KID).unwrap()]
    );
    assert_eq!(protobuf_kids(b"").unwrap(), Vec::<Vec<u8>>::new());
    for bad in [
        &b"\x00"[..],
        b"not-protobuf",
        &[0x08, 0x01, 0x12, 0x01],
        &[0x12],
    ] {
        assert_eq!(
            protobuf_kids(bad).unwrap_err(),
            PipelineError::Value("widevine-invalid-payload".to_string()),
            "{bad:?}"
        );
    }
    // a 64-bit and a 32-bit fixed field are skipped
    let skipped = [
        0x09, 0, 0, 0, 0, 0, 0, 0, 0, 0x15, 0, 0, 0, 0, 0x12, 0x01, 0x7f,
    ];
    assert_eq!(protobuf_kids(&skipped).unwrap(), vec![vec![0x7f]]);
}

#[test]
fn webvtt_cues_preserved_and_timestamp_map_tracks_transport_offset() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let source = tmp.path().join("s.vtt");
    let cue = "\n\n00:00:00.000 --> 00:00:01.000\nHello\n";
    fs::write(
        &source,
        format!("WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:8589900000{cue}"),
    )
    .unwrap();
    let track = track(
        "t000",
        "text",
        "webvtt",
        "webvtt",
        vec![segment(0, &source, 2.0)],
    );
    let result = copy_text(&track, &tmp.path().join("asset"), &Fake::default()).unwrap();
    let output = fs::read_to_string(&result.segments[0].path).unwrap();
    assert!(output.ends_with(cue), "{output}");
    assert!(output.contains(&format!(
        "MPEGTS:{}",
        (8_589_900_000_u64 + 180_000) % (1 << 33)
    )));
    assert!(!result.encrypted);
    assert_eq!(result.codec, "wvtt");
    assert_eq!(
        result.text_map_offset,
        Some(to_fraction(frac(8_589_900_000, 90000)))
    );
    assert_eq!(result.text_earliest_cue, Some(to_fraction(frac(0, 1))));
    assert_eq!(
        (
            result.segments[0].timescale,
            result.segments[0].duration,
            result.segments[0].presentation_start
        ),
        (90000, 180_000, 0)
    );
}

#[test]
fn webvtt_without_a_map_gets_the_common_offset_and_a_bom_is_dropped() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let source = tmp.path().join("s.vtt");
    fs::write(
        &source,
        "\u{feff}WEBVTT\r\n\r\n00:00:00.500 --> 00:00:01.000\r\nHello\r\n",
    )
    .unwrap();
    let track = track(
        "t000",
        "text",
        "webvtt",
        "webvtt",
        vec![segment(0, &source, 2.0)],
    );
    let result = copy_text(&track, &tmp.path().join("asset"), &Fake::default()).unwrap();
    let output = fs::read_to_string(&result.segments[0].path).unwrap();
    assert_eq!(
        output,
        "WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:180000\n\n00:00:00.500 --> 00:00:01.000\nHello\n"
    );
    assert_eq!(result.text_map_offset, Some(to_fraction(frac(0, 1))));
    assert_eq!(result.text_earliest_cue, Some(to_fraction(frac(1, 2))));
}

#[test]
fn webvtt_inconsistent_segment_clocks_fail_loud() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let mut segments = Vec::new();
    for (index, transport) in [0, 90000].into_iter().enumerate() {
        let path = tmp.path().join(format!("s{index}.vtt"));
        fs::write(&path, format!("WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:{transport}\n\n00:00:00.000 --> 00:00:01.000\nHello\n")).unwrap();
        segments.push(segment(i64::try_from(index).unwrap(), &path, 2.0));
    }
    let track = track("t000", "text", "webvtt", "webvtt", segments);
    assert_eq!(
        copy_text(&track, &tmp.path().join("asset"), &Fake::default()).unwrap_err(),
        TrackError::Media("unsupported-webvtt-clock-change".to_string())
    );
}

#[test]
fn webvtt_rejects_a_missing_header_two_maps_and_an_overflowing_clock() {
    let tmp = private_fixture::private_tempdir().unwrap();
    for (name, content) in [
        ("header", "WEBVT\n\n"),
        (
            "two-maps",
            "WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0\n\n",
        ),
        ("stray-map", "WEBVTT\n\nX-TIMESTAMP-MAP junk\n"),
        (
            "overflow",
            "WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:8589934592\n\n",
        ),
        ("nul", "WEBVTT\n\0\n"),
        ("one-line", "WEBVTT"),
    ] {
        let path = tmp.path().join(format!("{name}.vtt"));
        fs::write(&path, content).unwrap();
        let track = track(
            "t000",
            "text",
            "webvtt",
            "webvtt",
            vec![segment(0, &path, 2.0)],
        );
        assert_eq!(
            copy_text(&track, &tmp.path().join("asset"), &Fake::default()).unwrap_err(),
            TrackError::Media("unsupported-subtitle-profile".to_string()),
            "{name}"
        );
    }
}

#[test]
fn muxed_audio_selection_is_snapshot_bound_and_records_omissions() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let seg = segment(0, tmp.path().join("source.ts"), 2.0);
    let mut first = track("a000", "audio", "ts", "aac", vec![seg.clone()]);
    first.group_id = "muxed-audio".to_string();
    first.muxed = true;
    let mut second = first.clone();
    second.id = "a001".to_string();
    let analysis = analysis(tmp.path(), vec![first.clone(), second.clone()]);
    // decoded digests differ per track (the fake keys them on the work file's stem)
    let fake = Fake {
        mismatch: Some("a001".to_string()),
        ..Fake::default()
    };
    let digests = |fake: &Fake| selected_tracks(&analysis, None, fake);
    assert_eq!(
        digests(&fake).unwrap_err(),
        PipelineError::Value("audio-selection-required".to_string())
    );
    let selection =
        |analysis_id: &str, id: &str| PackagingSelection::new(analysis_id, "digest", Some(id));
    assert_eq!(
        selected_tracks(&analysis, Some(&selection("old", "a000")), &fake).unwrap_err(),
        PipelineError::Value("stale-packaging-selection".to_string())
    );
    assert_eq!(
        selected_tracks(&analysis, Some(&selection("hls-test", "foreign")), &fake).unwrap_err(),
        PipelineError::Value("invalid-audio-selection".to_string())
    );
    let (chosen, unused) =
        selected_tracks(&analysis, Some(&selection("hls-test", "a001")), &fake).unwrap();
    assert_eq!((chosen, unused), (vec![second], vec![first]));
}

#[test]
fn selection_values_compare_as_python_values() {
    // `--selection` is a JSON file: its members are whatever it holds (Python's dataclass
    // takes any object); only the analysis's own strings are current.
    let tmp = private_fixture::private_tempdir().unwrap();
    let seg = segment(0, tmp.path().join("source.ts"), 2.0);
    let mut first = track("a000", "audio", "ts", "aac", vec![seg.clone()]);
    first.group_id = "muxed-audio".to_string();
    first.muxed = true;
    let mut second = first.clone();
    second.id = "a001".to_string();
    let analysis = analysis(tmp.path(), vec![first.clone(), second.clone()]);
    let fake = Fake::default();
    let selection = |analysis_id: Value, audio: Value| PackagingSelection {
        analysis_id,
        snapshot_digest: json!("digest"),
        audio_source_id: audio,
    };
    let stale = PipelineError::Value("stale-packaging-selection".to_string());
    for not_text in [json!(null), json!(1), json!(["hls-test"])] {
        assert_eq!(
            selected_tracks(&analysis, Some(&selection(not_text, json!("a000"))), &fake)
                .unwrap_err(),
            stale
        );
    }
    for (audio, error) in [
        (json!(5), "invalid-audio-selection"),
        (json!(true), "invalid-audio-selection"),
        (json!(["a000"]), "unhashable type: 'list'"),
        (json!({"a000": 1}), "unhashable type: 'dict'"),
    ] {
        assert_eq!(
            selected_tracks(&analysis, Some(&selection(json!("hls-test"), audio)), &fake)
                .unwrap_err(),
            PipelineError::Value(error.to_string())
        );
    }
    // a falsy choice is no choice: equivalent candidates, the first is kept
    for falsy in [json!(null), json!(""), json!(0), json!(false), json!([])] {
        let (chosen, unused) =
            selected_tracks(&analysis, Some(&selection(json!("hls-test"), falsy)), &fake).unwrap();
        assert_eq!(
            (chosen, unused),
            (vec![first.clone()], vec![second.clone()])
        );
    }
}

#[test]
fn equivalent_muxed_audio_picks_the_first_without_a_selection() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let seg = segment(0, tmp.path().join("source.ts"), 2.0);
    let mut first = track("a000", "audio", "ts", "aac", vec![seg.clone()]);
    first.group_id = "muxed-audio".to_string();
    first.muxed = true;
    let mut second = first.clone();
    second.id = "a001".to_string();
    let video = track("v000", "video", "ts", "h264", vec![seg]);
    let analysis = analysis(
        tmp.path(),
        vec![video.clone(), first.clone(), second.clone()],
    );
    let (chosen, unused) = selected_tracks(&analysis, None, &Fake::default()).unwrap();
    assert_eq!((chosen, unused), (vec![video, first], vec![second]));
}

#[test]
fn declared_audio_groups_are_never_removed_by_global_selection() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let seg = segment(0, tmp.path().join("s.ts"), 2.0);
    let mut ja = track("a000", "audio", "ts", "aac", vec![seg.clone()]);
    ja.group_id = "ja".to_string();
    ja.muxed = true;
    let mut en = ja.clone();
    en.id = "a001".to_string();
    en.group_id = "en".to_string();
    let analysis = analysis(tmp.path(), vec![ja.clone(), en.clone()]);
    let (chosen, unused) = selected_tracks(&analysis, None, &Fake::default()).unwrap();
    assert_eq!((chosen, unused), (vec![ja.clone(), en.clone()], vec![]));
    // two candidates in one of several groups need a per-group choice
    let mut ja2 = ja.clone();
    ja2.id = "a002".to_string();
    let analysis = self::analysis(tmp.path(), vec![ja, en, ja2]);
    assert_eq!(
        selected_tracks(&analysis, None, &Fake::default()).unwrap_err(),
        PipelineError::Value("multiple-audio-group-selection-required".to_string())
    );
}

#[test]
fn imsc_encoded_doctype_never_reaches_xml_parser() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let init = tmp.path().join("init.mp4");
    fs::write(&init, b"init").unwrap();
    let fragment = tmp.path().join("segment.m4s");
    fs::write(&fragment, b"media").unwrap();
    let mut track = track(
        "t000",
        "text",
        "imsc",
        "ttml",
        vec![segment(0, &fragment, 2.0)],
    );
    track.init_path = Some(init);
    let xml = r#"<!DOCTYPE tt [<!ENTITY x "payload">]><tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttp="http://www.w3.org/ns/ttml#parameter" ttp:profile="http://www.w3.org/ns/ttml/profile/imsc1/text"><body>&x;</body></tt>"#;
    let utf16: Vec<u8> = [0xff, 0xfe]
        .into_iter()
        .chain(xml.encode_utf16().flat_map(u16::to_le_bytes))
        .collect();
    let fake = Fake {
        payloads: vec![utf16],
        ..Fake::default()
    };
    assert_eq!(
        copy_text(&track, &tmp.path().join("asset"), &fake).unwrap_err(),
        TrackError::Media("unsupported-subtitle-profile".to_string())
    );
    // a DOCTYPE in UTF-8, an image profile, an external reference and an empty payload list
    for bad in [
        xml.as_bytes().to_vec(),
        br#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttp="http://www.w3.org/ns/ttml#parameter" ttp:profile="http://www.w3.org/ns/ttml/profile/imsc1/image"><body/></tt>"#.to_vec(),
        br#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttp="http://www.w3.org/ns/ttml#parameter" ttp:profile="http://www.w3.org/ns/ttml/profile/imsc1/text"><body><div><image src="x.png"/></div></body></tt>"#.to_vec(),
        br#"<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttp="http://www.w3.org/ns/ttml#parameter" ttp:profile="http://www.w3.org/ns/ttml/profile/imsc1/text"><body><p xlink:href="x" xmlns:xlink="http://www.w3.org/1999/xlink" src="y"/></body></tt>"#.to_vec(),
    ] {
        let fake = Fake { payloads: vec![bad.clone()], ..Fake::default() };
        assert_eq!(copy_text(&track, &tmp.path().join("asset"), &fake).unwrap_err(), TrackError::Media("unsupported-subtitle-profile".to_string()), "{}", String::from_utf8_lossy(&bad));
    }
    let fake = Fake {
        payloads: vec![],
        ..Fake::default()
    };
    assert_eq!(
        copy_text(&track, &tmp.path().join("asset"), &fake).unwrap_err(),
        TrackError::Media("unsupported-subtitle-profile".to_string())
    );
}

#[test]
fn tracks_are_packaged_concurrently_and_reported_in_track_order() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let seg = segment(0, tmp.path().join("s.ts"), 2.0);
    let vtt = tmp.path().join("s.vtt");
    fs::write(&vtt, "WEBVTT\n\n00:00:00.000 --> 00:00:01.000\nHi\n").unwrap();
    let mut tracks: Vec<SourceTrack> = (0..3)
        .map(|i| track(&format!("v00{i}"), "video", "ts", "h264", vec![seg.clone()]))
        .collect();
    tracks.push(track(
        "t000",
        "text",
        "webvtt",
        "webvtt",
        vec![segment(0, &vtt, 2.0)],
    ));
    let analysis = analysis(tmp.path(), tracks);
    // every video track is in flight at the same time
    let fake = Fake {
        barrier: Some(Arc::new(Barrier::new(3))),
        mismatch: Some("v001".to_string()),
        ..Fake::default()
    };
    let records: Mutex<Vec<Record>> = Mutex::new(Vec::new());
    let on_log = |record: Record| records.lock().unwrap().push(record);
    let report = package_hls(
        &analysis,
        &keys(),
        &signaling(),
        None,
        Some(&on_log),
        "packager",
        Some(4),
        &fake,
    )
    .unwrap();
    let ids: Vec<&str> = report
        .track_results
        .iter()
        .map(|r| r.source_track_id.as_str())
        .collect();
    assert_eq!(ids, ["v000", "v001", "v002", "t000"]);
    let dispositions: Vec<&str> = report
        .track_results
        .iter()
        .map(|r| r.disposition.as_str())
        .collect();
    assert_eq!(dispositions, ["packaged", "failed", "packaged", "packaged"]);
    assert_eq!(
        report.findings,
        vec!["decoded-content-mismatch".to_string()]
    );
    assert!(!report.success);
    assert_eq!(
        report.track_results[1].checks,
        vec!["decoded-content-mismatch".to_string()]
    );
    assert_eq!(
        report.track_results[0].checks,
        [
            "source-correspondence-pass",
            "timing-pass",
            "decoded-identity-pass",
            "cbcs-signaling-pass"
        ]
    );
    assert_eq!(
        report.track_results[3].checks,
        ["text-payload-pass", "text-timing-map-pass"]
    );
    assert!(
        fake.threads.lock().unwrap().len() >= 3,
        "segments go through the shared pool"
    );
    assert_progress_records(&records.lock().unwrap());
    assert_failed_report_on_disk(&tmp.path().join("asset"));
}

/// The per-track progress records: one start and one outcome per track, carrying the
/// track's index and the total, keys in Python's insertion order.
fn assert_progress_records(records: &[Record]) {
    let progress: BTreeSet<(String, String, String)> = records
        .iter()
        .filter(|r| r.get("stage").and_then(|v| v.as_str()) == Some("package-hls"))
        .map(|r| {
            (
                r["message"].as_str().unwrap().to_string(),
                r["track_id"].as_str().unwrap().to_string(),
                r["level"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let expected: BTreeSet<(String, String, String)> = [
        ("track-started", "v000", "info"),
        ("track-started", "v001", "info"),
        ("track-started", "v002", "info"),
        ("track-started", "t000", "info"),
        ("track-packaged", "v000", "info"),
        ("track-failed", "v001", "error"),
        ("track-packaged", "v002", "info"),
        ("track-packaged", "t000", "info"),
    ]
    .into_iter()
    .map(|(m, t, l)| (m.to_string(), t.to_string(), l.to_string()))
    .collect();
    assert_eq!(progress, expected);
    assert!(
        records
            .iter()
            .all(|r| r["index"].as_u64().unwrap() < 4 && r["total"].as_u64() == Some(4))
    );
    assert_eq!(
        records[0].keys().cloned().collect::<Vec<_>>(),
        ["stage", "level", "message", "track_id", "index", "total"]
    );
}

/// No manifests after a failure; the report is on disk with the public shape.
fn assert_failed_report_on_disk(asset: &Path) {
    assert!(!asset.join("manifests/clear-hls").exists());
    let written: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(asset.join("reports/package-hls-summary.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(written["evidence"]["packaging"], "failed");
    assert_eq!(written["tracks"][0]["segments"][0]["sample_count"], 1);
    assert_eq!(
        written
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        [
            "schema_version",
            "report_kind",
            "analysis_id",
            "asset_id",
            "success",
            "production_verified",
            "manifest_paths",
            "tracks",
            "findings",
            "evidence"
        ]
    );
}

#[test]
fn a_clean_run_writes_both_manifests_and_a_successful_report() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let seg = segment(0, tmp.path().join("s.ts"), 2.0);
    let video = track("v000", "video", "ts", "h264", vec![seg.clone()]);
    let mut audio = track("a000", "audio", "fmp4", "aac", vec![seg]);
    audio.language = "ja".to_string();
    let analysis = analysis(tmp.path(), vec![video, audio]);
    let report = package_hls(
        &analysis,
        &keys(),
        &signaling(),
        None,
        None,
        "packager",
        Some(2),
        &Fake::default(),
    )
    .unwrap();
    assert!(report.success, "{:?}", report.findings);
    let names: Vec<String> = report
        .manifest_paths
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["manifest.mpd", "master.m3u8"]);
    assert!(
        tmp.path()
            .join("asset/manifests/clear-hls/manifest.mpd")
            .exists()
    );
    let text =
        fs::read_to_string(tmp.path().join("asset/reports/package-hls-summary.json")).unwrap();
    assert!(
        text.starts_with("{\n  \"schema_version\": \"1\",\n  \"report_kind\": \"package-hls\",\n")
            && text.ends_with("}\n"),
        "{text}"
    );
    assert!(text.contains("\"packaging\": \"measured-local\""));
    assert!(!text.contains(&"10".repeat(16)) && !text.contains(&"20".repeat(16)));
}

#[test]
fn manifest_value_and_os_errors_are_a_finding_and_others_propagate() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let seg = segment(0, tmp.path().join("s.ts"), 2.0);
    let analysis = analysis(
        tmp.path(),
        vec![track("v000", "video", "ts", "h264", vec![seg])],
    );
    let run = |error: PyErr| {
        let fake = Fake {
            dash_error: Some(error),
            ..Fake::default()
        };
        package_hls(
            &analysis,
            &keys(),
            &signaling(),
            None,
            None,
            "packager",
            Some(1),
            &fake,
        )
    };
    let report = run(PyErr::Value("bad origin".to_string())).unwrap();
    assert_eq!(report.findings, ["manifest-generation-failed"]);
    assert_eq!(report.manifest_paths, Vec::<PathBuf>::new());
    assert!(!tmp.path().join("asset/manifests/clear-hls").exists());
    // `except (ValueError, OSError)` only: a ZeroDivisionError escapes, a TypeError too
    assert_eq!(
        run(PyErr::ZeroDivision("division by zero".to_string())).unwrap_err(),
        PipelineError::Infrastructure("division by zero".to_string())
    );
    assert_eq!(
        run(PyErr::Other {
            class: "TypeError",
            message: "unsupported".to_string()
        })
        .unwrap_err(),
        PipelineError::Value("unsupported".to_string())
    );
}

#[test]
fn a_runtime_error_in_an_init_check_aborts_the_whole_run() {
    // `hls_pipeline`:473's except tuple misses RuntimeError / ToolError: an mp4dump failure
    // while validating an init segment is not a track failure but the run's (implemented as is).
    let tmp = private_fixture::private_tempdir().unwrap();
    let seg = segment(0, tmp.path().join("s.ts"), 2.0);
    let analysis = analysis(
        tmp.path(),
        vec![track("v000", "video", "ts", "h264", vec![seg])],
    );
    for workers in [1, 4] {
        let fake = Fake {
            init_probe_fails: true,
            ..Fake::default()
        };
        let error = package_hls(
            &analysis,
            &keys(),
            &signaling(),
            None,
            None,
            "packager",
            Some(workers),
            &fake,
        )
        .unwrap_err();
        assert!(
            matches!(error, PipelineError::Infrastructure(_)),
            "{workers}: {error:?}"
        );
    }
    assert!(
        !tmp.path()
            .join("asset/reports/package-hls-summary.json")
            .exists(),
        "no report: the exception left package_hls"
    );
}

#[test]
fn one_worker_packages_on_the_callers_thread_without_a_pool() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let seg = segment(0, tmp.path().join("s.ts"), 2.0);
    let analysis = analysis(
        tmp.path(),
        vec![track("v000", "video", "ts", "h264", vec![seg])],
    );
    let fake = Fake {
        failing: Some(0),
        ..Fake::default()
    };
    let report = package_hls(
        &analysis,
        &keys(),
        &signaling(),
        None,
        None,
        "packager",
        Some(1),
        &fake,
    )
    .unwrap();
    assert_eq!(
        report.findings,
        vec!["packager-encryption-failed".to_string()]
    );
    let threads = fake.threads.lock().unwrap().clone();
    assert_eq!(
        threads,
        HashSet::from([std::thread::current().id()]),
        "inline, on the caller's thread"
    );
    for out_of_bounds in [0, 65, -1] {
        let error = package_hls(
            &analysis,
            &keys(),
            &signaling(),
            None,
            None,
            "packager",
            Some(out_of_bounds),
            &fake,
        )
        .unwrap_err();
        assert_eq!(
            error,
            PipelineError::Value("max_workers must be 1..64".to_string())
        );
    }
}

#[test]
fn prologue_errors_keep_the_python_order_and_classes() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let seg = segment(0, tmp.path().join("s.ts"), 2.0);
    let mut analysis = analysis(
        tmp.path(),
        vec![track("v000", "video", "ts", "h264", vec![seg.clone()])],
    );
    let fake = Fake::default();
    let mut bad = signaling();
    bad.fairplay_uri = "https://nope".to_string();
    // signaling is validated before the admission flag
    analysis.admitted = false;
    assert_eq!(
        package_hls(
            &analysis,
            &keys(),
            &bad,
            None,
            None,
            "packager",
            Some(1),
            &fake
        )
        .unwrap_err(),
        PipelineError::Value("fairplay-invalid-uri".to_string())
    );
    assert_eq!(
        package_hls(
            &analysis,
            &keys(),
            &signaling(),
            None,
            None,
            "packager",
            Some(1),
            &fake
        )
        .unwrap_err(),
        PipelineError::Value("source-not-admitted".to_string())
    );
    analysis.admitted = true;
    let changed = Fake {
        snapshot_error: Some("source-snapshot-changed".to_string()),
        ..Fake::default()
    };
    assert_eq!(
        package_hls(
            &analysis,
            &keys(),
            &signaling(),
            None,
            None,
            "packager",
            Some(1),
            &changed
        )
        .unwrap_err(),
        PipelineError::Source("source-snapshot-changed".to_string())
    );
    let text_only = self::analysis(
        tmp.path(),
        vec![track("t000", "text", "webvtt", "webvtt", vec![seg])],
    );
    assert_eq!(
        package_hls(
            &text_only,
            &keys(),
            &signaling(),
            None,
            None,
            "packager",
            Some(1),
            &fake
        )
        .unwrap_err(),
        PipelineError::Value("source-no-av-tracks".to_string())
    );
}

fn ordering_case(pooled: bool, failing: Option<usize>) {
    let tmp = private_fixture::private_tempdir().unwrap();
    let count = 6;
    let segments: Vec<SourceSegment> = (0..count)
        .map(|i| {
            let path = tmp.path().join(format!("s{i}.ts"));
            fs::write(&path, [0x47; 188]).unwrap();
            segment(i64::try_from(i).unwrap(), path, 6.0)
        })
        .collect();
    let track = track("v000", "video", "ts", "h264", segments);
    let fake = Fake {
        failing,
        ..Fake::default()
    };
    let keys = keys();
    let signaling = signaling();
    let pr = validate_signaling(&signaling, &keys).unwrap();
    let job = TrackJob {
        track: &track,
        keys: &keys,
        signaling: &signaling,
        pr: &pr,
        packager_bin: "packager",
        deps: &fake,
    };
    let asset = tmp.path().join("asset");
    let outcome = if pooled {
        std::thread::scope(|scope| {
            let pool = Pool::new(scope, 4);
            package_track(job, &asset, Some(&pool))
        })
    } else {
        package_track(job, &asset, None)
    };
    let events = fake.events();
    let prepares: Vec<usize> = events
        .iter()
        .filter(|(k, _)| k == "prepare")
        .filter_map(|(_, n)| *n)
        .collect();
    let finishes: Vec<usize> = events
        .iter()
        .filter(|(k, _)| k == "finish")
        .filter_map(|(_, n)| *n)
        .collect();
    match failing {
        None => {
            let output = outcome.expect("packaged");
            let mut sorted_p = prepares.clone();
            sorted_p.sort_unstable();
            let mut sorted_f = finishes.clone();
            sorted_f.sort_unstable();
            assert_eq!(sorted_p, (0..count).collect::<Vec<_>>());
            assert_eq!(sorted_f, (0..count).collect::<Vec<_>>());
            let last_prepare = events
                .iter()
                .rposition(|(k, _)| k == "prepare" || k == "validate-init")
                .unwrap();
            let first_finish = events.iter().position(|(k, _)| k == "finish").unwrap();
            assert!(
                last_prepare < first_finish,
                "no decoded-identity step starts before every segment is prepared and its init validated"
            );
            assert_eq!(output.segments.len(), count);
            assert!(output.init_path.as_ref().unwrap().exists());
            assert_eq!(output.codec, "avc1.64001e");
            assert_eq!(output.frame_rate, "24/1");
            assert_eq!(
                (
                    output.language.as_str(),
                    output.role.as_str(),
                    output.name.as_str(),
                    output.group_id.as_str()
                ),
                ("und", "main", "v000", "audio")
            );
            if !pooled {
                assert_eq!(
                    prepares,
                    (0..count).collect::<Vec<_>>(),
                    "inline, the sequential order"
                );
                assert_eq!(finishes, (0..count).collect::<Vec<_>>());
            }
        }
        Some(number) => {
            assert_eq!(
                outcome.unwrap_err(),
                TrackError::Media("packager-encryption-failed".to_string())
            );
            assert!(
                prepares.contains(&number) && finishes.is_empty(),
                "a failed prepare blocks every finish"
            );
        }
    }
}

#[test]
fn every_prepare_ends_before_any_finish_starts_inline() {
    ordering_case(false, None);
}

#[test]
fn every_prepare_ends_before_any_finish_starts_pooled() {
    ordering_case(true, None);
}

#[test]
fn a_failed_prepare_blocks_them_all_inline() {
    ordering_case(false, Some(1));
}

#[test]
fn a_failed_prepare_blocks_them_all_pooled() {
    ordering_case(true, Some(1));
}

#[test]
fn unsafe_track_ids_fail_before_packaging_writes() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let escaped = tmp.path().join("escaped");
    let source = tmp.path().join("source.vtt");
    fs::write(&source, "WEBVTT\n\n00:00.000 --> 00:01.000\nHello\n").unwrap();
    let ids = [
        escaped.to_string_lossy().into_owned(),
        "../escaped".into(),
        "nested/track".into(),
        "nested\\track".into(),
        "C:escaped".into(),
        ".".into(),
        "..".into(),
        String::new(),
        "bad\0id".into(),
    ];
    for id in ids {
        for container in ["webvtt", "fmp4"] {
            let text = track(
                &id,
                "text",
                container,
                "wvtt",
                vec![segment(0, &source, 1.0)],
            );
            let fake = Fake::default();
            let asset = tmp.path().join("asset");
            assert_eq!(
                copy_text(&text, &asset, &fake).unwrap_err(),
                TrackError::Media("invalid-track-id".into()),
                "{id:?}"
            );
            assert!(!asset.exists());
            assert!(!escaped.exists());
            assert_eq!(fake.events(), []);
        }
        let video = track(&id, "video", "fmp4", "avc1", vec![segment(0, &source, 1.0)]);
        let key = keys();
        let signaling = signaling();
        let pr = validate_signaling(&signaling, &key).unwrap();
        let fake = Fake::default();
        let job = TrackJob {
            track: &video,
            keys: &key,
            signaling: &signaling,
            pr: &pr,
            packager_bin: "packager",
            deps: &fake,
        };
        assert_eq!(
            package_track(job, &tmp.path().join("asset"), None).unwrap_err(),
            TrackError::Media("invalid-track-id".into())
        );
        assert!(!tmp.path().join("asset").exists());
        assert!(!escaped.exists());
        assert_eq!(fake.events(), []);
    }
}

#[test]
fn package_preflight_rejects_all_track_ids_before_clearing_manifests() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let bad = track("../escaped", "text", "webvtt", "wvtt", vec![]);
    let analyzed = analysis(tmp.path(), vec![bad]);
    let manifest = tmp.path().join("asset/manifests/clear-hls/keep.mpd");
    fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    fs::write(&manifest, "keep").unwrap();
    let fake = Fake {
        snapshot_error: Some("must-not-run".into()),
        ..Fake::default()
    };
    assert_eq!(
        package_hls(
            &analyzed,
            &keys(),
            &signaling(),
            None,
            None,
            "packager",
            Some(1),
            &fake
        )
        .unwrap_err(),
        PipelineError::Value("invalid-track-id".into())
    );
    assert_eq!(fs::read_to_string(manifest).unwrap(), "keep");
    assert_eq!(fake.events(), []);
}

#[cfg(unix)]
#[test]
fn text_outputs_refuse_existing_directory_and_leaf_symlinks() {
    use std::os::unix::fs::symlink;
    for leaf in [false, true] {
        let tmp = private_fixture::private_tempdir().unwrap();
        let source = tmp.path().join("source.vtt");
        fs::write(&source, "WEBVTT\n\n00:00.000 --> 00:01.000\nHello\n").unwrap();
        let victim = tmp.path().join("victim");
        fs::create_dir(&victim).unwrap();
        let marker = victim.join("seg_0.vtt");
        fs::write(&marker, "unchanged").unwrap();
        let folder = tmp.path().join("asset/cbcs/t0");
        fs::create_dir_all(folder.parent().unwrap()).unwrap();
        if leaf {
            fs::create_dir(&folder).unwrap();
            symlink(&marker, folder.join("seg_0.vtt")).unwrap();
        } else {
            symlink(&victim, &folder).unwrap();
        }
        let text = track(
            "t0",
            "text",
            "webvtt",
            "wvtt",
            vec![segment(0, &source, 1.0)],
        );
        assert_eq!(
            copy_text(&text, &tmp.path().join("asset"), &Fake::default()).unwrap_err(),
            TrackError::Media("unsafe-output-path".into())
        );
        assert_eq!(fs::read_to_string(marker).unwrap(), "unchanged");
    }
}

#[cfg(unix)]
#[test]
fn media_outputs_refuse_a_symlinked_work_directory_before_creating_outputs() {
    use std::os::unix::fs::symlink;
    let tmp = private_fixture::private_tempdir().unwrap();
    let asset = tmp.path().join("asset");
    let victim = tmp.path().join("victim");
    fs::create_dir(&victim).unwrap();
    fs::create_dir_all(asset.join("clear/package-hls")).unwrap();
    symlink(&victim, asset.join("clear/package-hls/v0")).unwrap();
    let video = track("v0", "video", "fmp4", "avc1", vec![]);
    let key = keys();
    let signaling = signaling();
    let pr = validate_signaling(&signaling, &key).unwrap();
    let fake = Fake::default();
    let job = TrackJob {
        track: &video,
        keys: &key,
        signaling: &signaling,
        pr: &pr,
        packager_bin: "packager",
        deps: &fake,
    };
    assert_eq!(
        package_track(job, &asset, None).unwrap_err(),
        TrackError::Media("unsafe-output-path".into())
    );
    assert!(!asset.join("cbcs").exists());
    assert!(fs::read_dir(victim).unwrap().next().is_none());
    assert_eq!(fake.events(), []);
}

#[cfg(windows)]
use private_fixture::windows;
#[cfg(windows)]
#[test]
fn text_outputs_refuse_windows_directory_junctions() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let source = tmp.path().join("source.vtt");
    fs::write(&source, "WEBVTT\n\n00:00.000 --> 00:01.000\nHello\n").unwrap();
    let victim = tmp.path().join("victim");
    fs::create_dir(&victim).unwrap();
    let marker = victim.join("seg_0.vtt");
    fs::write(&marker, "unchanged").unwrap();
    let folder = tmp.path().join("asset/cbcs/t0");
    fs::create_dir_all(folder.parent().unwrap()).unwrap();
    windows::junction(&folder, &victim);
    let text = track(
        "t0",
        "text",
        "webvtt",
        "wvtt",
        vec![segment(0, &source, 1.0)],
    );
    assert_eq!(
        copy_text(&text, &tmp.path().join("asset"), &Fake::default()).unwrap_err(),
        TrackError::Media("unsafe-output-path".into())
    );
    assert_eq!(fs::read_to_string(marker).unwrap(), "unchanged");
    fs::remove_dir(folder).unwrap();
}
#[cfg(windows)]
#[test]
fn media_outputs_refuse_windows_work_directory_junctions_before_outputs() {
    let tmp = private_fixture::private_tempdir().unwrap();
    let asset = tmp.path().join("asset");
    let victim = tmp.path().join("victim");
    fs::create_dir(&victim).unwrap();
    fs::create_dir_all(asset.join("clear/package-hls")).unwrap();
    let link = asset.join("clear/package-hls/v0");
    windows::junction(&link, &victim);
    let video = track("v0", "video", "fmp4", "avc1", vec![]);
    let key = keys();
    let signaling = signaling();
    let pr = validate_signaling(&signaling, &key).unwrap();
    let fake = Fake::default();
    let job = TrackJob {
        track: &video,
        keys: &key,
        signaling: &signaling,
        pr: &pr,
        packager_bin: "packager",
        deps: &fake,
    };
    assert_eq!(
        package_track(job, &asset, None).unwrap_err(),
        TrackError::Media("unsafe-output-path".into())
    );
    assert!(!asset.join("cbcs").exists());
    assert!(fs::read_dir(victim).unwrap().next().is_none());
    assert_eq!(fake.events(), []);
    fs::remove_dir(link).unwrap();
}
