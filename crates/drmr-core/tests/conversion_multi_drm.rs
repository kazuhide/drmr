//! Every DRM output is checked from its final manifest and retained clear transport source.
use drmr_core::{
    conversion::{
        ConversionRequest, Converter, HlsInputKeys, InputKeys, OutputSignaling,
        VerificationRequest, VerificationSource, key_file,
    },
    events::{Collector, NoSink},
    inspection::{OutputFormat, TargetProtection},
    toolrun::{RunOptions, Secrets, ToolRunner},
};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const IV: &str = "202122232425262728292a2b2c2d2e2f";
const LICENSE: &str = "https://example.invalid/license?tenant=SYNTHETIC-PRIVATE-METADATA";
const URI: &str = "skd://synthetic-private-content";
fn path(value: &Path) -> String {
    drmr_core::manifest_support::path::fspath(value).unwrap()
}
fn pssh() -> String {
    hex::encode(
        drmr_core::playready::build_pssh_box(
            &drmr_core::playready::WIDEVINE_SYSTEM_ID,
            &hex::decode(format!("1210{KID}")).unwrap(),
        )
        .unwrap(),
    )
}
fn source(root: &Path) -> PathBuf {
    drmr_core::native::create_media_dir_all(root).unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let argv = vec![
        "ffmpeg".into(),
        "-v".into(),
        "error".into(),
        "-nostdin".into(),
        "-threads".into(),
        "1".into(),
        "-i".into(),
        path(&clip),
        "-map".into(),
        "0:v:0".into(),
        "-map".into(),
        "0:a:0".into(),
        "-c".into(),
        "copy".into(),
        "-hls_time".into(),
        "2".into(),
        "-hls_playlist_type".into(),
        "vod".into(),
        "-hls_segment_filename".into(),
        path(&root.join("segment-%d.ts")),
        path(&root.join("media.m3u8")),
    ];
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink));
    let result = runner
        .run_tool(
            &argv,
            "ffmpeg",
            &RunOptions {
                timeout: Some(Duration::from_secs(30)),
                max_output_chars: 4096,
                log_stdout: false,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        result.returncode,
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    root.join("media.m3u8")
}
fn keys(root: &Path, name: &str, output: &serde_json::Value) -> key_file::OperatorKeys {
    let path = root.join(format!("{name}.json"));
    drmr_core::native::create_private_new(&path)
        .unwrap()
        .write_all(
            serde_json::json!({"schema_version":1,"output":output})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
    key_file::read(&path).unwrap()
}
fn converter(events: &Arc<Collector>) -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
        "packager",
        "ffmpeg",
    )
}
fn copy_manifests(root: &Path, current: &Path, destination: &Path) {
    for entry in std::fs::read_dir(current).unwrap() {
        let path = entry.unwrap().path();
        let metadata = path.symlink_metadata().unwrap();
        assert!(!metadata.is_symlink());
        if metadata.is_dir() {
            copy_manifests(root, &path, destination);
        } else if matches!(
            path.extension().and_then(|value| value.to_str()),
            Some("mpd" | "m3u8")
        ) {
            let target = destination.join(path.strip_prefix(root).unwrap());
            drmr_core::native::create_private_dir_all(target.parent().unwrap()).unwrap();
            drmr_core::native::create_private_new(&target)
                .unwrap()
                .write_all(&std::fs::read(&path).unwrap())
                .unwrap();
        }
    }
}
fn retain_manifests(root: &Path, name: &str) {
    let Some(destination) = std::env::var_os("DRMR_TEST_MANIFEST_EVIDENCE") else {
        return;
    };
    let destination = PathBuf::from(destination).join(name);
    drmr_core::native::create_private_dir_new(&destination).unwrap();
    copy_manifests(root, root, &destination);
}
fn assert_label_order(entrypoint: &Path) {
    let original = std::fs::read_to_string(entrypoint).unwrap();
    let document = roxmltree::Document::parse(&original).unwrap();
    for set in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "AdaptationSet")
    {
        let children: Vec<_> = set
            .children()
            .filter(roxmltree::Node::is_element)
            .map(|node| node.tag_name().name())
            .collect();
        if let Some(label) = children.iter().position(|name| *name == "Label") {
            assert!(
                children
                    .iter()
                    .enumerate()
                    .filter(|(_, name)| **name == "ContentProtection")
                    .all(|(position, _)| position < label),
                "DASH ContentProtection must precede Label"
            );
        }
    }
}
fn reject_mutations(
    events: &Arc<Collector>,
    output: &Path,
    entrypoint: &Path,
    format: OutputFormat,
    target: TargetProtection,
    keys: &key_file::OperatorKeys,
) {
    if format == OutputFormat::Dash {
        let original = std::fs::read_to_string(entrypoint).unwrap();
        for changed in [
            original.replace("value=\"cbcs\"", "value=\"cenc\""),
            original.replace(
                "00010203-0405-0607-0809-0a0b0c0d0e0f",
                "10010203-0405-0607-0809-0a0b0c0d0e0f",
            ),
            original.replace("urn:mpeg:cenc:2013", "urn:example:wrong-cenc"),
        ] {
            assert_ne!(changed, original);
            std::fs::write(entrypoint, changed).unwrap();
            assert!(
                converter(events)
                    .verify_output_signaling(entrypoint, format, target, keys)
                    .is_err()
            );
        }
        std::fs::write(entrypoint, original).unwrap();
    } else {
        let playlist = output.join("track-0000/media.m3u8");
        let original = std::fs::read_to_string(&playlist).unwrap();
        let version = original
            .lines()
            .find(|line| line.starts_with("#EXT-X-VERSION:"))
            .unwrap();
        let lower = format!(
            "#EXT-X-VERSION:{}",
            if format == OutputFormat::Hls { 5 } else { 4 }
        );
        for changed in [
            original.replace(URI, "skd://different-private-content"),
            original.replace("KEYFORMATVERSIONS=\"1\"", "KEYFORMATVERSIONS=\"2\""),
            original.replace("METHOD=SAMPLE-AES", "METHOD=AES-128"),
            original.replace(version, &lower),
            original.replace(&format!("{version}\n"), ""),
            original.replace(version, &format!("{version}\n{version}")),
            original.replace(version, "#EXT-X-VERSION:invalid"),
        ] {
            assert_ne!(changed, original);
            std::fs::write(&playlist, changed).unwrap();
            assert!(
                converter(events)
                    .verify_output_signaling(entrypoint, format, target, keys)
                    .is_err()
            );
        }
        std::fs::write(&playlist, original).unwrap();
    }
}
fn output_cases() -> [(
    &'static str,
    OutputFormat,
    TargetProtection,
    serde_json::Value,
); 3] {
    [
        (
            "dash",
            OutputFormat::Dash,
            TargetProtection::Cbcs,
            serde_json::json!({"kid":KID,"key":KEY,"signaling":{"system":"multi-drm","widevine_pssh":pssh(),"playready_license_url":LICENSE}}),
        ),
        (
            "fmp4",
            OutputFormat::Hls,
            TargetProtection::Cbcs,
            serde_json::json!({"kid":KID,"key":KEY,"hls_key_uri":URI,"signaling":{"system":"multi-drm","fairplay":true}}),
        ),
        (
            "ts",
            OutputFormat::HlsTs,
            TargetProtection::HlsSampleAes,
            serde_json::json!({"key":KEY,"iv":IV,"hls_key_uri":URI,"signaling":{"system":"multi-drm","fairplay":true}}),
        ),
    ]
}
#[test]
fn clear_transport_has_independently_verified_dash_and_fairplay_outputs() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let input = source(&owner.path().join("source"));
    let events = Arc::new(Collector::default());
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    for (name, format, target, raw) in output_cases() {
        let keys = keys(owner.path(), name, &raw);
        let output = owner.path().join(name);
        let command = converter(&events).with_output_signaling(keys.signaling.clone());
        let report = command
            .convert(ConversionRequest {
                input: &input,
                output: &output,
                format,
                target,
                input_keys: &empty,
                hls_input_keys: &hls,
                output_key: keys.output.as_ref(),
                hls_key_uri: keys
                    .hls_output_uri()
                    .filter(|_| format != OutputFormat::HlsTs),
                hls_output_key: keys.hls_output.as_ref(),
            })
            .unwrap();
        assert!(report.verification.passed);
        assert!(!report.verification.license_playback_verified);
        let entrypoint = output.join(if format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        });
        if format == OutputFormat::Dash {
            assert_label_order(&entrypoint);
        }
        converter(&events)
            .verify_output_signaling(&entrypoint, format, target, &keys)
            .unwrap();
        let result_keys = keys.output_verification_keys().unwrap();
        let result_hls = keys.output_hls_verification_keys().unwrap();
        let verified = converter(&events)
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &input,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
                output: VerificationSource {
                    input: &entrypoint,
                    input_keys: &result_keys,
                    hls_keys: &result_hls,
                },
            })
            .unwrap();
        assert!(verified.passed);
        assert!(verified.checks.packet_payloads_equal);
        assert!(verified.checks.packet_timing_equal);
        assert!(verified.checks.decoded_content_equal);
        reject_mutations(&events, &output, &entrypoint, format, target, &keys);
        converter(&events)
            .verify_output_signaling(&entrypoint, format, target, &keys)
            .unwrap();
        retain_manifests(&output, name);
        println!(
            "DRMR_MULTI_DRM_CELL {}",
            serde_json::json!({"format":format,"protection":target,"systems":keys.signaling.as_ref().unwrap().systems(),"media_verified":true,"independent_signaling_verified":true,"independent_media_verified":true,"packet_payloads_equal":true,"packet_timing_equal":true,"decoded_content_equal":true,"license_playback_verified":false})
        );
    }
    let rendered = format!("{:?}", events.records());
    for private in [KEY, IV, LICENSE, URI, pssh().as_str()] {
        assert!(!rendered.contains(private));
    }
    assert!(OutputSignaling::multi_drm(Some(&pssh()), None, false).is_ok());
}
