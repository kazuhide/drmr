//! Explicit `PlayReady` metadata is qualified independently of media decryption.
use std::{
    io::Write as _,
    path::Path,
    process::{Command, Output},
};

const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const SOURCE_KEY: &str = "202122232425262728292a2b2c2d2e2f";
const LICENSE: &str = "https://example.invalid/license?operator=SYNTHETIC-PRIVATE-METADATA";

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(args)
        .output()
        .unwrap()
}
fn path(value: &Path) -> String {
    drmr_core::manifest_support::path::fspath(value).unwrap()
}
fn verified(report: &serde_json::Value) {
    assert_eq!(report["media_verified"], true);
    assert_eq!(report["license_playback_verified"], false);
    assert_eq!(report["checks"]["stream_metadata"], "pass");
    for check in [
        "packet_payloads_equal",
        "packet_timing_equal",
        "decoded_content_equal",
    ] {
        assert_eq!(report["checks"][check], true);
    }
}

fn coded_packets(input: &Path, video_only: bool) -> serde_json::Value {
    let mut probe = Command::new("ffprobe");
    probe.args([
        "-v",
        "error",
        "-show_packets",
        "-show_entries",
        "packet=stream_index,pts,dts,duration,data_hash",
        "-show_data_hash",
        "sha256",
        "-of",
        "json",
    ]);
    if video_only {
        probe.args(["-select_streams", "v:0"]);
    }
    let output = probe.arg(input).output().unwrap();
    assert!(output.status.success());
    let mut packets: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    for packet in packets["packets"].as_array_mut().unwrap() {
        // The original progressive clip has a finite AAC end-padding declaration.
        // This derived fragmented baseline qualifies coded bytes and clocks, not that edit.
        packet.as_object_mut().unwrap().retain(|key, _| {
            ["stream_index", "pts", "dts", "duration", "data_hash"].contains(&key.as_str())
        });
    }
    let rows = packets["packets"].as_array_mut().unwrap();
    for row in rows.iter() {
        assert_eq!(row.as_object().unwrap().len(), 5);
        assert!(
            row["data_hash"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("SHA256:") && hash.len() == 71)
        );
        for field in ["stream_index", "pts", "dts", "duration"] {
            assert!(row[field].as_i64().is_some());
        }
    }
    rows.sort_by_key(|row| row["stream_index"].as_u64().unwrap());
    packets
}

fn qualified_profile(audio: bool) {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path().join("media");
    drmr_core::native::create_media_dir_all(&root).unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let before = std::fs::read(&clip).unwrap();
    // Independently remux the attributed video without opaque encoder metadata.
    // No timestamp offset or trimming is applied to the complete 240-frame stream.
    let input = source(&root, audio, false);
    let original_packets = coded_packets(&clip, !audio);
    let source_packets = coded_packets(&input, !audio);
    assert_eq!(
        source_packets["packets"].as_array().unwrap().len(),
        if audio { 710 } else { 240 }
    );
    assert_eq!(original_packets, source_packets);
    let source_before = std::fs::read(&input).unwrap();
    // Operator secrets require the strict parent, not the inheritable media ACL.
    let keys = owner.path().join("keys.json");
    let config = serde_json::json!({"schema_version":1,"input":{"keys":[{"kid":"ffeeddccbbaa99887766554433221100","key":SOURCE_KEY}]},"output":{"kid":KID,"key":KEY,
        "signaling":{"system":"playready","license_url":LICENSE}}})
    .to_string();
    drmr_core::native::create_private_new(&keys)
        .unwrap()
        .write_all(config.as_bytes())
        .unwrap();
    assert!(drmr_core::native::open_private_read(&keys).is_ok());
    let output = root.join("result");
    let result = cli(&[
        "convert",
        &path(&input),
        "--to",
        "cbcs",
        "--format",
        "dash",
        "--output",
        &path(&output),
        "--keys-file",
        &path(&keys),
        "--max-workers",
        "1",
        "--json",
    ]);
    assert!(
        result.status.success(),
        "conversion exit {:?}; stderr {}",
        result.status.code(),
        String::from_utf8_lossy(&result.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    verified(&report["verification"]);
    let objects = assert_signaling(&output, audio);
    let verified_result = cli(&[
        "verify",
        &path(&output.join("manifest.mpd")),
        "--source",
        &path(&input),
        "--keys-file",
        &path(&keys),
        "--max-workers",
        "1",
        "--json",
    ]);
    assert!(
        verified_result.status.success(),
        "independent verify exit {:?}; stderr {}",
        verified_result.status.code(),
        String::from_utf8_lossy(&verified_result.stderr)
    );
    verified(&serde_json::from_slice::<serde_json::Value>(&verified_result.stdout).unwrap());
    assert_video_count(&input);
    assert_private(
        &[
            &result.stdout,
            &result.stderr,
            &verified_result.stdout,
            &verified_result.stderr,
        ],
        &objects,
    );
    assert_eq!(std::fs::read(&clip).unwrap(), before);
    assert_eq!(std::fs::read(&input).unwrap(), source_before);
}

#[test]
fn explicit_playready_video_dash_preserves_media_and_signaling() {
    qualified_profile(false);
}
#[test]
fn explicit_playready_joint_dash_preserves_media_and_signaling() {
    qualified_profile(true);
}

fn source(root: &Path, audio: bool, empty: bool) -> std::path::PathBuf {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let input = root.join("source.mp4");
    let mut remux = Command::new("ffmpeg");
    remux.args(["-v", "error", "-i", &path(&clip), "-map", "0:v:0"]);
    if audio {
        remux.args(["-map", "0:a:0"]);
    }
    assert!(
        remux
            .args([
                "-c",
                "copy",
                "-map_metadata",
                "-1",
                "-bitexact",
                "-movflags",
                if empty {
                    "empty_moov+default_base_moof+frag_keyframe"
                } else {
                    "delay_moov+default_base_moof+frag_keyframe"
                },
                &path(&input)
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    input
}

#[test]
fn unrepresentable_joint_dash_duration_is_refused_before_publication() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path().join("media");
    drmr_core::native::create_media_dir_all(&root).unwrap();
    let input = source(&root, true, true);
    let before = std::fs::read(&input).unwrap();
    // Operator secrets require the strict parent, not the inheritable media ACL.
    let keys = owner.path().join("keys.json");
    let body = serde_json::json!({"schema_version":1,"output":{"kid":KID,"key":KEY,
        "signaling":{"system":"playready","license_url":LICENSE}}});
    drmr_core::native::create_private_new(&keys)
        .unwrap()
        .write_all(body.to_string().as_bytes())
        .unwrap();
    assert!(drmr_core::native::open_private_read(&keys).is_ok());
    let output = root.join("refused");
    let result = cli(&[
        "convert",
        &path(&input),
        "--to",
        "cbcs",
        "--format",
        "dash",
        "--output",
        &path(&output),
        "--keys-file",
        &path(&keys),
        "--json",
    ]);
    assert_eq!(result.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("dash-presentation-duration-mismatch")
    );
    assert!(!output.exists());
    assert_eq!(std::fs::read(input).unwrap(), before);
}

#[test]
fn explicit_signaling_rejects_unknown_profiles_and_existing_metadata() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path().join("media");
    drmr_core::native::create_media_dir_all(&root).unwrap();
    let input = source(&root, false, false);
    let before = std::fs::read(&input).unwrap();
    // Operator secrets require the strict parent, not the inheritable media ACL.
    let keys = owner.path().join("keys.json");
    let mut config = serde_json::json!({"schema_version":1,"output":{"kid":KID,"key":KEY,
        "signaling":{"system":"playready","license_url":LICENSE}}});
    drmr_core::native::create_private_new(&keys)
        .unwrap()
        .write_all(config.to_string().as_bytes())
        .unwrap();
    assert!(drmr_core::native::open_private_read(&keys).is_ok());
    let output = root.join("refused");
    for (target, format) in [
        ("cenc", "dash"),
        ("clear", "dash"),
        ("cbcs", "hls"),
        ("cbcs", "mp4"),
        ("hls-sample-aes", "hls-ts"),
    ] {
        let result = cli(&[
            "convert",
            &path(&input),
            "--to",
            target,
            "--format",
            format,
            "--output",
            &path(&output),
            "--keys-file",
            &path(&keys),
            "--packager-bin",
            "unused-no-tool-should-start",
            "--json",
        ]);
        assert!(
            String::from_utf8_lossy(&result.stderr)
                .contains("output-signaling-profile-not-supported")
        );
        assert!(!output.exists());
    }
    config["output"]["signaling"]["system"] = "unknown".into();
    std::fs::write(&keys, config.to_string()).unwrap();
    let invalid = cli(&[
        "convert",
        &path(&input),
        "--to",
        "cbcs",
        "--format",
        "dash",
        "--output",
        &path(&output),
        "--keys-file",
        &path(&keys),
        "--json",
    ]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("key-file-invalid"));
    config["output"]["signaling"]["system"] = "playready".into();
    std::fs::write(&keys, config.to_string()).unwrap();
    let pssh = drmr_core::playready::build_playready_v43(KID, LICENSE)
        .unwrap()
        .pssh_box;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&input)
        .unwrap()
        .write_all(&pssh)
        .unwrap();
    let protected = std::fs::read(&input).unwrap();
    let invalid = cli(&[
        "convert",
        &path(&input),
        "--to",
        "cbcs",
        "--format",
        "dash",
        "--output",
        &path(&output),
        "--keys-file",
        &path(&keys),
        "--packager-bin",
        "unused-no-tool-should-start",
        "--json",
    ]);
    assert!(
        String::from_utf8_lossy(&invalid.stderr).contains("output-signaling-profile-not-supported")
    );
    assert!(!output.exists());
    assert_eq!(std::fs::read(&input).unwrap(), protected);
    assert_eq!(&protected[..before.len()], before);
}

fn assert_signaling(output: &Path, audio: bool) -> drmr_core::playready::PlayReadyObjects {
    let objects = drmr_core::playready::build_playready_v43(KID, LICENSE).unwrap();
    let manifest = std::fs::read_to_string(output.join("manifest.mpd")).unwrap();
    assert!(manifest.contains(&format!("<cenc:pssh>{}</cenc:pssh>", objects.pssh_b64)));
    assert!(manifest.contains(&format!("<mspr:pro>{}</mspr:pro>", objects.pro_b64)));
    for track in 0..if audio { 2 } else { 1 } {
        let init = std::fs::read(output.join(format!("track-{track:04}/init.mp4"))).unwrap();
        let boxes = drmr_core::mp4::parse_boxes(&init).unwrap();
        let pssh: Vec<_> = drmr_core::mp4::walk(&boxes)
            .into_iter()
            .filter(|node| node.kind() == *b"pssh")
            .collect();
        assert_eq!(pssh.len(), 1);
        assert_eq!(pssh[0].header.bytes(&init), objects.pssh_box);
    }
    let video_segments = std::fs::read_dir(output.join("track-0000"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "m4s"))
        .count();
    assert_eq!(video_segments, 5);
    let parsed = drmr_core::playready::parse_pro(&objects.pro_b64).unwrap();
    assert_eq!(parsed.version, "4.3.0.0");
    assert_eq!(parsed.algid.as_deref(), Some("AESCBC"));
    assert_eq!(parsed.kid_values_b64, ["AwIBAAUEBwYICQoLDA0ODw=="]);
    assert!(!parsed.wrmheader_xml.contains("CHECKSUM"));
    assert_eq!(parsed.la_url.as_deref(), Some(LICENSE));
    objects
}

fn assert_private(outputs: &[&[u8]], objects: &drmr_core::playready::PlayReadyObjects) {
    for bytes in outputs {
        let text = String::from_utf8_lossy(bytes);
        for private in [
            KEY,
            SOURCE_KEY,
            LICENSE,
            &objects.pro_b64,
            &objects.pssh_b64,
            &objects.pssh_hex,
        ] {
            assert!(!text.contains(private));
        }
    }
}

fn assert_video_count(input: &Path) {
    let probe = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_packets",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=nb_read_packets,nb_read_frames",
            "-of",
            "json",
            &path(input),
        ])
        .output()
        .unwrap();
    let profile: serde_json::Value = serde_json::from_slice(&probe.stdout).unwrap();
    assert_eq!(profile["streams"][0]["nb_read_packets"], "240");
    assert_eq!(profile["streams"][0]["nb_read_frames"], "240");
}
