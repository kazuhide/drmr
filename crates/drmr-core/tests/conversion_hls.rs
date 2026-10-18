use drmr_core::conversion::{
    Aes128Key, ContentKey, Converter, HlsInputKeys, HlsOutputKey, InputKeys, OutputKey,
};
use drmr_core::events::{Collector, NoSink};
use drmr_core::inspection::{Protection, TargetProtection};
use drmr_core::toolrun::{Secrets, ToolRunner};
use std::{fmt::Write as _, path::Path, sync::Arc};
const URI: &str = "https://operator.invalid/SECRET-key";
const KEY: &str = "42424242424242424242424242424242";
const KID: &str = "000102030405060708090a0b0c0d0e0f";
fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "ffmpeg",
    )
}
fn video(path: &Path) {
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x64:rate=5",
            "-t",
            "3",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-g",
            "5",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}
#[test]
fn mp4_to_aes128_hls_and_back_preserves_all_media_for_each_mp4_scheme() {
    let directory = tempfile::tempdir().unwrap();
    let clear = directory.path().join("clear.mp4");
    video(&clear);
    let converter = converter();
    let output_key = HlsOutputKey::new(URI, Aes128Key::from_hex(KEY).unwrap()).unwrap();
    let hls_keys =
        HlsInputKeys::new(vec![(URI.into(), Aes128Key::from_hex(KEY).unwrap())]).unwrap();
    for protection in [
        TargetProtection::Clear,
        TargetProtection::Cenc,
        TargetProtection::Cbcs,
    ] {
        let source = directory
            .path()
            .join(format!("source-{}.mp4", protection.as_str()));
        let key = OutputKey::from_hex(KID, KEY).unwrap();
        converter
            .convert_mp4(
                &clear,
                &source,
                protection,
                &InputKeys::default(),
                (protection != TargetProtection::Clear).then_some(&key),
            )
            .unwrap();
        let input_keys = InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap();
        let hls = directory
            .path()
            .join(format!("hls-{}", protection.as_str()));
        let report = converter
            .convert_hls_aes128(&source, &hls, &input_keys, &output_key)
            .unwrap();
        assert!(report.verification.passed);
        assert_eq!(
            serde_json::to_value(&report).unwrap()["processing"]["route"],
            "fmp4-hls-presentation"
        );
        assert_eq!(report.output.protection, Protection::HlsAes128);
        assert!(!serde_json::to_string(&report).unwrap().contains("SECRET"));
        assert!(
            !std::fs::read_dir(&hls).unwrap().any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains("key"))
        );
        for target in [
            TargetProtection::Clear,
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
        ] {
            let destination = directory.path().join(format!(
                "{}-from-hls-{}.mp4",
                protection.as_str(),
                target.as_str()
            ));
            let output_key = OutputKey::from_hex(KID, KEY).unwrap();
            let report = converter
                .convert_from_hls(
                    &hls.join("media.m3u8"),
                    &destination,
                    target,
                    &InputKeys::default(),
                    &hls_keys,
                    (target != TargetProtection::Clear).then_some(&output_key),
                )
                .unwrap();
            assert!(report.verification.passed);
            assert_eq!(
                serde_json::to_value(&report).unwrap()["processing"]["input_preparation"],
                "local-hls-resources"
            );
        }
    }
}
#[test]
fn aes128_hls_wrong_or_missing_mapping_never_fetches_key_uri_or_publishes_output() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    video(&source);
    let hls = directory.path().join("hls");
    let converter = converter();
    converter
        .convert_hls_aes128(
            &source,
            &hls,
            &InputKeys::default(),
            &HlsOutputKey::new(URI, Aes128Key::from_hex(KEY).unwrap()).unwrap(),
        )
        .unwrap();
    let output = directory.path().join("out.mp4");
    for keys in [
        HlsInputKeys::default(),
        HlsInputKeys::new(vec![(
            URI.into(),
            Aes128Key::from_hex(&"00".repeat(16)).unwrap(),
        )])
        .unwrap(),
    ] {
        assert!(
            converter
                .convert_from_hls(
                    &hls.join("media.m3u8"),
                    &output,
                    TargetProtection::Clear,
                    &InputKeys::default(),
                    &keys,
                    None
                )
                .is_err()
        );
        assert!(!output.exists());
    }
}
#[test]
fn key_uri_iv_and_secret_never_reach_hls_conversion_records() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    video(&source);
    let sink = Arc::new(Collector::default());
    let converter = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), sink.clone())),
        "packager",
        "ffmpeg",
    );
    converter
        .convert_hls_aes128(
            &source,
            &directory.path().join("hls"),
            &InputKeys::default(),
            &HlsOutputKey::new(URI, Aes128Key::from_hex(KEY).unwrap()).unwrap(),
        )
        .unwrap();
    let records = serde_json::to_string(&sink.records()).unwrap();
    assert!(!records.contains("SECRET"));
    assert!(!records.contains(KEY));
    assert!(!records.contains("IV="));
}

#[test]
fn aes128_to_aes128_uses_separate_key_roles_and_fresh_explicit_vectors() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    video(&source);
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    let converter = converter();
    converter
        .convert_hls_aes128(
            &source,
            &first,
            &InputKeys::default(),
            &HlsOutputKey::new(URI, Aes128Key::from_hex(KEY).unwrap()).unwrap(),
        )
        .unwrap();
    let input_keys =
        HlsInputKeys::new(vec![(URI.into(), Aes128Key::from_hex(KEY).unwrap())]).unwrap();
    let report = converter
        .convert_hls_to_hls_aes128(
            &first.join("media.m3u8"),
            &second,
            &InputKeys::default(),
            &input_keys,
            &HlsOutputKey::new(
                "https://operator.invalid/new",
                Aes128Key::from_hex(&"12".repeat(16)).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    assert!(report.verification.passed);
    assert_ne!(
        std::fs::read(first.join("segment-000001.m4s")).unwrap(),
        std::fs::read(second.join("segment-000001.m4s")).unwrap()
    );
}
#[test]
fn encrypted_map_requires_explicit_iv_and_resource_escapes_fail_before_publication() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = directory.path().join("media.m3u8");
    std::fs::write(directory.path().join("init.mp4"), [0; 16]).unwrap();
    let keys = HlsInputKeys::new(vec![(URI.into(), Aes128Key::from_hex(KEY).unwrap())]).unwrap();
    for (uri, key, expected) in [
        (
            "init.mp4",
            format!("#EXT-X-KEY:METHOD=AES-128,URI=\"{URI}\"\n"),
            "hls-encrypted-map-explicit-iv-required",
        ),
        (
            "../outside.mp4",
            String::new(),
            "hls-resource-not-confined-local",
        ),
        (
            "https://remote.invalid/init.mp4",
            String::new(),
            "hls-resource-not-confined-local",
        ),
    ] {
        std::fs::write(
            &manifest,
            format!("#EXTM3U\n{key}#EXT-X-MAP:URI=\"{uri}\"\n#EXTINF:1,\nx\n#EXT-X-ENDLIST\n"),
        )
        .unwrap();
        let output = directory.path().join("out.mp4");
        assert_eq!(
            converter()
                .convert_from_hls(
                    &manifest,
                    &output,
                    TargetProtection::Clear,
                    &InputKeys::default(),
                    &keys,
                    None
                )
                .unwrap_err()
                .0,
            expected
        );
        assert!(!output.exists());
    }
}

#[test]
fn imported_key_rotation_and_implicit_sequence_vectors_preserve_media() {
    use drmr_core::conversion::{SegmentIv, decrypt_segment, encrypt_segment};
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    video(&source);
    let hls = directory.path().join("hls");
    let converter = converter();
    converter
        .convert_hls_aes128(
            &source,
            &hls,
            &InputKeys::default(),
            &HlsOutputKey::new(URI, Aes128Key::from_hex(KEY).unwrap()).unwrap(),
        )
        .unwrap();
    let other_uri = "https://operator.invalid/second";
    let other_key = "13131313131313131313131313131313";
    let playlist = std::fs::read_to_string(hls.join("media.m3u8")).unwrap();
    let mut rewritten = String::new();
    let mut current = String::new();
    let mut index = 0u64;
    for line in playlist.lines() {
        if line.starts_with("#EXT-X-KEY:") {
            current = line.into();
            continue;
        }
        if line.starts_with("#EXT-X-MAP:") {
            rewritten.push_str(&current);
            rewritten.push('\n');
        }
        if line.starts_with("#EXT-X-MEDIA-SEQUENCE:") {
            rewritten.push_str("#EXT-X-MEDIA-SEQUENCE:42\n");
            continue;
        }
        if !line.starts_with('#') && !line.is_empty() {
            let attrs =
                drmr_core::m3u8::parse_attributes(current.strip_prefix("#EXT-X-KEY:").unwrap())
                    .unwrap();
            let iv = SegmentIv::from_hex(attrs.get("IV").unwrap()).unwrap();
            let clear = directory.path().join(format!("clear-{index}"));
            decrypt_segment(
                &hls.join(line),
                &clear,
                &Aes128Key::from_hex(KEY).unwrap(),
                &iv,
            )
            .unwrap();
            let (uri, key) = if index == 0 {
                (URI, KEY)
            } else {
                (other_uri, other_key)
            };
            let cipher = directory.path().join(format!("cipher-{index}"));
            encrypt_segment(
                &clear,
                &cipher,
                &Aes128Key::from_hex(key).unwrap(),
                &SegmentIv::for_sequence(42 + index),
            )
            .unwrap();
            std::fs::rename(cipher, hls.join(line)).unwrap();
            writeln!(rewritten, "#EXT-X-KEY:METHOD=AES-128,URI=\"{uri}\"").unwrap();
            index += 1;
        }
        rewritten.push_str(line);
        rewritten.push('\n');
    }
    assert!(index >= 2);
    std::fs::write(hls.join("media.m3u8"), rewritten).unwrap();
    let keys = HlsInputKeys::new(vec![
        (URI.into(), Aes128Key::from_hex(KEY).unwrap()),
        (other_uri.into(), Aes128Key::from_hex(other_key).unwrap()),
    ])
    .unwrap();
    let report = converter
        .convert_from_hls(
            &hls.join("media.m3u8"),
            &directory.path().join("out.mp4"),
            TargetProtection::Clear,
            &InputKeys::default(),
            &keys,
            None,
        )
        .unwrap();
    assert!(report.verification.passed);
}
