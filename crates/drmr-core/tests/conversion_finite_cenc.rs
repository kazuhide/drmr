//! Genuine two-key finite presentations are verified against the complete original media.
mod common;

use drmr_core::{
    conversion::{
        ContentKey, ConversionReport, ConversionRequest, Converter, HlsInputKeys, InputKeys,
        VerificationReport, VerificationRequest, VerificationSource,
    },
    events::Collector,
    inspection::{OutputFormat, Protection, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

// Public fixture values only. Keys and identifiers deliberately do not overlap.
const VIDEO_ID: &str = "000102030405060708090a0b0c0d0e0f";
const AUDIO_ID: &str = "303132333435363738393a3b3c3d3e3f";
const VIDEO_KEY: &str = "101112131415161718191a1b1c1d1e1f";
const AUDIO_KEY: &str = "202122232425262728292a2b2c2d2e2f";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}

fn keys(entries: &[(&str, &str)]) -> InputKeys {
    InputKeys::new(
        entries
            .iter()
            .map(|(id, key)| ContentKey::from_hex(id, key).unwrap())
            .collect(),
    )
    .unwrap()
}

fn complete_keys() -> InputKeys {
    keys(&[(VIDEO_ID, VIDEO_KEY), (AUDIO_ID, AUDIO_KEY)])
}

fn converter() -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
    // A qualified progressive route must not need a fragmented-media intermediary.
    (
        Converter::new(runner, "unused-packager-for-progressive-profile", "ffmpeg"),
        events,
    )
}

fn convert(
    c: &Converter,
    source: &Path,
    output: &Path,
    input_keys: &InputKeys,
) -> Result<ConversionReport, drmr_core::conversion::ConversionError> {
    c.convert(ConversionRequest {
        input: source,
        output,
        format: OutputFormat::Mp4,
        target: TargetProtection::Clear,
        input_keys,
        hls_input_keys: &HlsInputKeys::default(),
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    })
}

fn verify(
    c: &Converter,
    source: &Path,
    output: &Path,
    input_keys: &InputKeys,
) -> Result<VerificationReport, drmr_core::conversion::ConversionError> {
    c.verify(VerificationRequest {
        source: VerificationSource {
            input: source,
            input_keys,
            hls_keys: &HlsInputKeys::default(),
        },
        output: VerificationSource {
            input: output,
            input_keys: &InputKeys::default(),
            hls_keys: &HlsInputKeys::default(),
        },
    })
}

fn assert_verified(report: &VerificationReport) {
    assert!(report.passed && report.media_verified, "{report:?}");
    assert!(!report.license_playback_verified);
    assert_eq!(
        serde_json::to_value(report).unwrap()["checks"]["stream_metadata"],
        "pass"
    );
    assert!(report.checks.packet_payloads_equal);
    assert!(report.checks.packet_timing_equal);
    assert!(report.checks.decoded_content_equal);
}

fn dictionary() -> String {
    format!("{VIDEO_ID}={VIDEO_KEY}:{AUDIO_ID}={AUDIO_KEY}")
}

fn tool(program: &str, input: &Path, encrypted: bool, arguments: &[&str]) -> Vec<u8> {
    let mut command = Command::new(program);
    command.args(["-v", "error", "-threads", "1"]);
    if program == "ffmpeg" {
        command.args(["-nostdin", "-xerror"]);
    }
    if encrypted {
        command.args(["-decryption_keys", &dictionary()]);
    }
    if program == "ffmpeg" {
        command.arg("-i");
    }
    command.arg(input).args(arguments);
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "independent {program} oracle failed: {:?}",
        output.status.code()
    );
    output.stdout
}

fn packets(input: &Path, encrypted: bool) -> Vec<Value> {
    let bytes = tool(
        "ffprobe",
        input,
        encrypted,
        &[
            "-show_packets",
            "-show_entries",
            "packet=stream_index,pts,dts,duration,data_hash:packet_side_data=",
            "-show_data_hash",
            "sha256",
            "-of",
            "json",
        ],
    );
    let records = serde_json::from_slice::<Value>(&bytes).unwrap()["packets"]
        .as_array()
        .unwrap()
        .clone();
    for packet in &records {
        for clock in ["pts", "dts", "duration"] {
            assert!(
                packet[clock].as_i64().is_some(),
                "missing absolute packet clock: {clock}"
            );
        }
        assert!(
            packet["data_hash"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("SHA256:"))
        );
    }
    records
}

fn assert_whole_media(input: &Path, encrypted: bool) {
    let original = fixture("clip.mp4");
    let expected = packets(&original, false);
    let actual = packets(input, encrypted);
    assert_eq!(actual.len(), 710);
    for (index, count) in [(0, 240), (1, 470)] {
        let select = |records: Vec<Value>| {
            records
                .into_iter()
                .filter(|p| p["stream_index"] == index)
                .collect::<Vec<_>>()
        };
        let actual = select(actual.clone());
        assert_eq!(actual.len(), count);
        assert_eq!(actual, select(expected.clone()));
    }
    let audio = [
        "-map",
        "0:a:0",
        "-threads",
        "1",
        "-c:a",
        "pcm_s32le",
        "-f",
        "s32le",
        "-",
    ];
    let pcm = tool("ffmpeg", input, encrypted, &audio);
    assert_eq!(pcm.len(), 480_000 * 2 * 4);
    assert_eq!(pcm, tool("ffmpeg", &original, false, &audio));
    let video = [
        "-map",
        "0:v:0",
        "-threads",
        "1",
        "-pix_fmt",
        "yuv420p",
        "-f",
        "framehash",
        "-hash",
        "sha256",
        "-",
    ];
    let frames = |bytes: Vec<u8>| {
        String::from_utf8(bytes)
            .unwrap()
            .lines()
            .filter(|s| !s.starts_with('#'))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let decoded = frames(tool("ffmpeg", input, encrypted, &video));
    assert_eq!(decoded.len(), 240);
    assert_eq!(decoded, frames(tool("ffmpeg", &original, false, &video)));
    assert_windows_equal(&original, input);
}

fn read32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn windows(path: &Path) -> Vec<(Vec<u8>, u32, i32, u32, u32)> {
    let data = std::fs::read(path).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&data).unwrap();
    let all = drmr_core::mp4::walk(&boxes);
    let movie = all
        .iter()
        .find(|n| n.kind() == *b"mvhd")
        .unwrap()
        .header
        .payload(&data);
    assert_eq!(movie[0], 0);
    let scale = read32(movie, 12);
    let mut result = Vec::new();
    for track in all.iter().filter(|n| n.kind() == *b"trak") {
        let nodes = drmr_core::mp4::walk(&track.children);
        let payload = |kind| {
            nodes
                .iter()
                .find(|n| n.kind() == kind)
                .unwrap()
                .header
                .payload(&data)
        };
        let media = payload(*b"mdhd");
        let header = payload(*b"tkhd");
        assert_eq!(media[0], 0);
        assert_eq!(header[0], 0);
        let edit = nodes
            .iter()
            .find(|n| n.kind() == *b"elst")
            .map(|n| n.header.payload(&data));
        let (time, duration) = match edit {
            Some(e) => {
                assert_eq!(e[0], 0);
                assert_eq!(read32(e, 4), 1);
                (
                    i32::from_be_bytes(e[12..16].try_into().unwrap()),
                    read32(e, 8),
                )
            }
            None => (0, read32(header, 20)),
        };
        result.push((
            payload(*b"hdlr")[8..12].to_vec(),
            read32(media, 12),
            time,
            duration,
            scale,
        ));
    }
    result.sort();
    result
}

fn assert_windows_equal(source: &Path, output: &Path) {
    let a = windows(source);
    let b = windows(output);
    assert_eq!(a.len(), b.len());
    for (a, b) in a.iter().zip(&b) {
        assert_eq!((&a.0, a.1, a.2), (&b.0, b.1, b.2));
        assert_eq!(
            u64::from(a.3) * u64::from(b.4),
            u64::from(b.3) * u64::from(a.4)
        );
    }
}

#[test]
fn both_iv_sizes_convert_and_independently_verify_every_original_track() {
    let root = common::private_tempdir().unwrap();
    let keys = complete_keys();
    let (c, events) = converter();
    for iv in [8, 16] {
        let source = fixture(&format!("cenc-iv{iv}.mp4"));
        let before = std::fs::read(&source).unwrap();
        assert_whole_media(&source, true);
        let output = root.path().join(format!("clear-iv{iv}.mp4"));
        let report = convert(&c, &source, &output, &keys).unwrap();
        assert_eq!(
            serde_json::to_value(&report).unwrap()["processing"]["route"],
            "progressive-mp4"
        );
        assert_eq!(
            serde_json::to_value(&report).unwrap()["processing"]["required_tools"],
            serde_json::json!(["ffmpeg", "ffprobe"])
        );
        assert_eq!(report.output.protection, Protection::Clear);
        assert_eq!(report.output.tracks.len(), 2);
        assert_verified(&report.verification);
        assert_verified(&verify(&c, &source, &output, &keys).unwrap());
        assert_verified(
            &c.verify_mp4(&source, &keys, &output, &InputKeys::default())
                .unwrap(),
        );
        assert_whole_media(&output, false);
        let bytes = std::fs::read(&output).unwrap();
        let boxes = drmr_core::mp4::parse_boxes(&bytes).unwrap();
        for node in drmr_core::mp4::walk(&boxes) {
            assert!(!matches!(
                &node.kind(),
                b"encv"
                    | b"enca"
                    | b"sinf"
                    | b"schi"
                    | b"tenc"
                    | b"senc"
                    | b"saiz"
                    | b"saio"
                    | b"pssh"
                    | b"moof"
                    | b"mvex"
            ));
        }
        assert_eq!(std::fs::read(&source).unwrap(), before);
        let retained = format!("{keys:?} {report:?} {:?}", events.records());
        for secret in [VIDEO_KEY, AUDIO_KEY, &dictionary()] {
            assert!(
                !retained.contains(secret),
                "secret retained in public diagnostics"
            );
        }
    }
}

#[test]
fn a_single_encrypted_aac_track_retains_its_complete_finite_audio() {
    let root = common::private_tempdir().unwrap();
    let input = root.path().join("audio.mp4");
    // The fixed IV is public fixture data, never a production encryption setting.
    let generated = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-copyts", "-i"])
        .arg(fixture("clip.mp4"))
        .args([
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-avoid_negative_ts",
            "disabled",
            "-use_editlist",
            "1",
            "-fflags",
            "+bitexact",
            "-encryption_scheme",
            "cenc-aes-ctr",
            "-encryption_kid",
            AUDIO_ID,
            "-encryption_key",
            AUDIO_KEY,
        ])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "single-track fixture generation failed"
    );
    let before = std::fs::read(&input).unwrap();
    let output = root.path().join("audio-clear.mp4");
    let (c, _) = converter();
    let keys = keys(&[(AUDIO_ID, AUDIO_KEY)]);
    let report = convert(&c, &input, &output, &keys).unwrap();
    assert_eq!(report.output.tracks.len(), 1);
    assert_verified(&report.verification);
    assert_verified(&verify(&c, &input, &output, &keys).unwrap());
    let expected = packets(&fixture("clip.mp4"), false)
        .into_iter()
        .filter(|p| p["stream_index"] == 1)
        .map(|mut p| {
            p["stream_index"] = Value::from(0);
            p
        })
        .collect::<Vec<_>>();
    assert_eq!(expected.len(), 470);
    assert_eq!(packets(&input, true), expected);
    assert_eq!(packets(&output, false), expected);
    let args = [
        "-map",
        "0:a:0",
        "-threads",
        "1",
        "-c:a",
        "pcm_s32le",
        "-f",
        "s32le",
        "-",
    ];
    let original_pcm = tool("ffmpeg", &fixture("clip.mp4"), false, &args);
    assert_eq!(original_pcm.len(), 480_000 * 2 * 4);
    assert_eq!(tool("ffmpeg", &input, true, &args), original_pcm);
    assert_eq!(tool("ffmpeg", &output, false, &args), original_pcm);
    assert_windows_equal(&input, &output);
    assert_eq!(std::fs::read(&input).unwrap(), before);
}

#[test]
fn the_mp4_specific_verifier_still_refuses_a_valid_manifest_in_either_role() {
    let root = common::private_tempdir().unwrap();
    // Relative child outputs also keep the default init segment beside the playlist on Windows.
    let generated = Command::new("ffmpeg")
        .current_dir(root.path())
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(fixture("clip.mp4"))
        .args([
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-hls_segment_type",
            "fmp4",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
            "segment-%d.m4s",
            "media.m3u8",
        ])
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "manifest fixture generation failed"
    );
    let manifest = root.path().join("media.m3u8");
    let (c, _) = converter();
    let empty = InputKeys::default();
    assert_verified(&verify(&c, &manifest, &manifest, &empty).unwrap());
    let source = fixture("cenc-iv8.mp4");
    let keys = complete_keys();
    for (input, input_keys, output, output_keys) in [
        (manifest.as_path(), &empty, source.as_path(), &keys),
        (source.as_path(), &keys, manifest.as_path(), &empty),
    ] {
        assert_eq!(
            c.verify_mp4(input, input_keys, output, output_keys)
                .unwrap_err()
                .0,
            "input-format-not-supported"
        );
    }
}

#[test]
fn incomplete_wrong_and_swapped_keys_refuse_publication_without_poisoning_the_converter() {
    let root = common::private_tempdir().unwrap();
    let source = fixture("cenc-iv8.mp4");
    let before = std::fs::read(&source).unwrap();
    let (c, events) = converter();
    let wrong = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    for (name, bad) in [
        ("empty", InputKeys::default()),
        ("video-only", keys(&[(VIDEO_ID, VIDEO_KEY)])),
        ("audio-only", keys(&[(AUDIO_ID, AUDIO_KEY)])),
        (
            "wrong-video",
            keys(&[(VIDEO_ID, wrong), (AUDIO_ID, AUDIO_KEY)]),
        ),
        (
            "wrong-audio",
            keys(&[(VIDEO_ID, VIDEO_KEY), (AUDIO_ID, wrong)]),
        ),
        (
            "swapped",
            keys(&[(VIDEO_ID, AUDIO_KEY), (AUDIO_ID, VIDEO_KEY)]),
        ),
    ] {
        let output = root.path().join(format!("{name}.mp4"));
        let error = convert(&c, &source, &output, &bad).unwrap_err();
        if matches!(name, "empty" | "video-only" | "audio-only") {
            assert_eq!(error.0, "input-key-required", "{name}");
        } else {
            assert!(
                matches!(
                    error.0,
                    "media-tool-failed" | "round-trip-verification-failed"
                ),
                "{name}: {error}"
            );
        }
        assert!(!output.exists(), "{name}");
        assert_eq!(std::fs::read(&source).unwrap(), before);
    }
    let sentinel = root.path().join("existing.mp4");
    std::fs::write(&sentinel, b"existing output must survive").unwrap();
    let good = complete_keys();
    assert!(convert(&c, &source, &sentinel, &good).is_err());
    assert_eq!(
        std::fs::read(&sentinel).unwrap(),
        b"existing output must survive"
    );
    let output = root.path().join("recovered.mp4");
    assert_verified(&convert(&c, &source, &output, &good).unwrap().verification);
    assert_verified(&verify(&c, &source, &output, &good).unwrap());
    let diagnostics = format!("{good:?} {:?}", events.records());
    for secret in [VIDEO_KEY, AUDIO_KEY, wrong, &dictionary()] {
        assert!(!diagnostics.contains(secret));
    }
}

fn patch_track(source: &Path, output: &Path, kind: [u8; 4], offset: usize, replacement: &[u8]) {
    let mut data = std::fs::read(source).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&data).unwrap();
    let node = drmr_core::mp4::walk(&boxes)
        .into_iter()
        .find(|n| n.kind() == kind)
        .unwrap();
    let at = node.header.offset + node.header.header_size + offset;
    data[at..at + replacement.len()].copy_from_slice(replacement);
    std::fs::write(output, data).unwrap();
}

fn shorten_video(source: &Path, output: &Path) {
    let mut data = std::fs::read(source).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&data).unwrap();
    let all = drmr_core::mp4::walk(&boxes);
    let movie_scale = read32(
        all.iter()
            .find(|n| n.kind() == *b"mvhd")
            .unwrap()
            .header
            .payload(&data),
        12,
    );
    let video_scale = read32(
        all.iter()
            .find(|n| n.kind() == *b"mdhd")
            .unwrap()
            .header
            .payload(&data),
        12,
    );
    let (mut a, mut b) = (movie_scale, video_scale);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    // The smallest duration change representable in both integer clocks remains inside one picture.
    let step = movie_scale / a;
    let video = all.iter().find(|n| n.kind() == *b"trak").unwrap();
    for node in drmr_core::mp4::walk(&video.children) {
        let offset = match &node.kind() {
            b"tkhd" => 20,
            b"elst" => 8,
            _ => continue,
        };
        assert_eq!(node.header.payload(&data)[0], 0);
        let at = node.header.offset + node.header.header_size + offset;
        let old = read32(&data, at);
        data[at..at + 4].copy_from_slice(&(old - step).to_be_bytes());
    }
    std::fs::write(output, data).unwrap();
}

#[test]
fn encrypted_reference_and_clear_candidate_keep_finite_windows_and_display_extents() {
    let root = common::private_tempdir().unwrap();
    let source = fixture("cenc-iv16.mp4");
    let output = root.path().join("clear.mp4");
    let keys = complete_keys();
    let (c, _) = converter();
    assert_verified(&convert(&c, &source, &output, &keys).unwrap().verification);
    for (role, baseline) in [
        ("source", source.as_path()),
        ("candidate", output.as_path()),
    ] {
        for change in ["window", "width"] {
            let mutant = root.path().join(format!("{role}-{change}.mp4"));
            if change == "window" {
                shorten_video(baseline, &mutant);
            } else {
                patch_track(
                    baseline,
                    &mutant,
                    *b"tkhd",
                    76,
                    &(640_u32 << 16).to_be_bytes(),
                );
            }
            assert_eq!(
                packets(baseline, role == "source"),
                packets(&mutant, role == "source")
            );
            let report = if role == "source" {
                verify(&c, &mutant, &output, &keys)
            } else {
                verify(&c, &source, &mutant, &keys)
            }
            .unwrap();
            assert!(!report.passed && !report.media_verified, "{role}-{change}");
            assert_eq!(
                serde_json::to_value(&report).unwrap()["checks"]["stream_metadata"],
                "pass"
            );
            assert!(report.checks.packet_payloads_equal);
            assert!(report.checks.decoded_content_equal);
            assert!(!report.checks.packet_timing_equal);
        }
        let gain = root.path().join(format!("{role}-gain.mp4"));
        patch_track(baseline, &gain, *b"mvhd", 24, &128_u16.to_be_bytes());
        let verified = if role == "source" {
            verify(&c, &gain, &output, &keys)
        } else {
            verify(&c, &source, &gain, &keys)
        };
        assert_eq!(verified.unwrap_err().0, "presentation-window-not-supported");
        if role == "source" {
            let destination = root.path().join("gain-output.mp4");
            let before = std::fs::read(&gain).unwrap();
            assert!(convert(&c, &gain, &destination, &keys).is_err());
            assert!(!destination.exists());
            assert_eq!(std::fs::read(&gain).unwrap(), before);
        }
    }
}

fn assert_admission_refused(input: &Path, reason: &str) {
    let root = common::private_tempdir().unwrap();
    let output = root.path().join("refused.mp4");
    let before = std::fs::read(input).unwrap();
    let (c, events) = converter();
    let error = convert(&c, input, &output, &complete_keys()).unwrap_err();
    assert_eq!(error.0, reason);
    assert!(!output.exists());
    assert_eq!(std::fs::read(input).unwrap(), before);
    assert!(
        events.records().is_empty(),
        "unsupported input reached an external tool"
    );
}

#[test]
fn top_level_drm_initialization_is_refused_even_when_its_system_is_unknown_or_malformed() {
    let root = common::private_tempdir().unwrap();
    let original = std::fs::read(fixture("cenc-iv8.mp4")).unwrap();
    let common_system = [
        0x10, 0x77, 0xef, 0xec, 0xc0, 0xb2, 0x4d, 0x02, 0xac, 0xe3, 0x3c, 0x1e, 0x52, 0xe2, 0xfb,
        0x4b,
    ];
    for (name, id, malformed, copies) in [
        ("known", common_system, false, 1),
        ("unknown", [0x55; 16], false, 1),
        ("malformed", [0x55; 16], true, 1),
        ("multiple", [0x55; 16], false, 2),
    ] {
        let mut bytes = original.clone();
        for _ in 0..copies {
            bytes.extend(32_u32.to_be_bytes());
            bytes.extend(b"pssh");
            bytes.extend([0; 4]);
            bytes.extend(id);
            bytes.extend(if malformed { u32::MAX } else { 0 }.to_be_bytes());
        }
        let input = root.path().join(format!("{name}.mp4"));
        std::fs::write(&input, bytes).unwrap();
        assert_admission_refused(&input, "progressive-cenc-profile-not-supported");
    }
}

#[test]
fn actual_auxiliary_mismatches_are_refused_before_foreign_execution() {
    let root = common::private_tempdir().unwrap();
    let original = fixture("cenc-iv8.mp4");
    for (name, kind, at, replacement) in [
        ("tenc-flags", *b"tenc", 3, vec![1]),
        ("tenc-iv", *b"tenc", 7, vec![7]),
        ("senc-count", *b"senc", 4, 239_u32.to_be_bytes().to_vec()),
        ("saiz-count", *b"saiz", 5, 239_u32.to_be_bytes().to_vec()),
        ("saio-offset", *b"saio", 15, vec![0xda]),
    ] {
        let input = root.path().join(format!("{name}.mp4"));
        patch_track(&original, &input, kind, at, &replacement);
        // An impossible IV size is already unclassified by the existing container inspector.
        let reason = if name == "tenc-iv" {
            "input-protection-not-supported"
        } else {
            "progressive-cenc-profile-not-supported"
        };
        assert_admission_refused(&input, reason);
    }
    let mut bytes = std::fs::read(&original).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&bytes).unwrap();
    let senc = drmr_core::mp4::walk(&boxes)
        .into_iter()
        .find(|b| b.kind() == *b"senc")
        .unwrap();
    // Full-box header, sample count, first IV, subsample count and clear-byte count.
    let at = senc.header.offset + senc.header.header_size + 4 + 4 + 8 + 2 + 2;
    let old = read32(&bytes, at);
    bytes[at..at + 4].copy_from_slice(&(old + 1).to_be_bytes());
    let input = root.path().join("subsample-total.mp4");
    std::fs::write(&input, bytes).unwrap();
    assert_admission_refused(&input, "progressive-cenc-profile-not-supported");
}

fn exclusive_lock(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.share_mode(0);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    file.try_lock().map_err(std::io::Error::other)?;
    Ok(file)
}

#[test]
fn cancellation_of_the_direct_mux_drains_its_descendant_before_returning() {
    use drmr_core::concurrency::Stop;
    use std::time::{Duration, Instant};
    let root = common::private_tempdir().unwrap();
    let ready = root.path().join("ready");
    let lock = root.path().join("held.lock");
    let argv = root.path().join("mux-argv");
    let release = root.path().join("release");
    let tool = drmr_test_support::install(
        &root.path().join("mux"),
        &drmr_test_support::ToolSpec::default()
            .with("argv_file", argv.to_str().unwrap())
            .with(
                "descendant_pid_file",
                root.path().join("descendant.pid").to_str().unwrap(),
            )
            .with("descendant_lock_file", lock.to_str().unwrap())
            .with("ready_file", ready.to_str().unwrap())
            .with("release_file", release.to_str().unwrap()),
    )
    .unwrap();
    let stop = Stop::new();
    let child_stop = stop.clone();
    let source = fixture("cenc-iv8.mp4");
    let before = std::fs::read(&source).unwrap();
    let output = root.path().join("cancelled.mp4");
    let worker_source = source.clone();
    let worker_output = output.clone();
    let worker = std::thread::spawn(move || {
        let events = Arc::new(Collector::default());
        let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
        let c =
            Converter::new(runner, "unused-packager", tool.to_str().unwrap()).with_stop(child_stop);
        let result = convert(&c, &worker_source, &worker_output, &complete_keys());
        (result, events.records())
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready.is_file() && !worker.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let became_ready = ready.is_file();
    let held = became_ready && exclusive_lock(&lock).is_err();
    // Always cancel and join, including fixture-readiness failures.
    stop.set();
    let (result, events) = worker.join().unwrap();
    let token = result.as_ref().err().map(|error| error.0);
    assert!(
        became_ready && held,
        "direct mux did not reach its held descendant barrier; result={token:?}"
    );
    assert_eq!(token, Some("cancelled"));
    let _released = exclusive_lock(&lock)
        .expect("descendant must release its lock before cancellation returns");
    let arguments = std::fs::read_to_string(&argv).unwrap();
    assert!(arguments.lines().any(|s| s == "-decryption_keys"));
    assert!(arguments.lines().any(|s| s == "-copyts"));
    assert!(arguments.lines().any(|s| s == "-movie_timescale"));
    assert!(
        Path::new(arguments.lines().last().unwrap())
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"))
    );
    assert!(!output.exists());
    assert_eq!(std::fs::read(source).unwrap(), before);
    let diagnostics = format!("{events:?}");
    for secret in [VIDEO_KEY, AUDIO_KEY, &dictionary()] {
        assert!(!diagnostics.contains(secret));
    }
}
