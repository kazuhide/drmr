//! Every DRM output is checked from its final manifest and retained clear transport source.
use drmr_core::{
    conversion::{
        ConversionReport, ConversionRequest, Converter, HlsInputKeys, InputKeys, OutputSignaling,
        VerificationRequest, VerificationSource, key_file,
    },
    events::{Collector, NoSink},
    inspection::{OutputFormat, TargetProtection},
    toolrun::{RunOptions, Secrets, ToolRunner},
};
use sha2::Digest as _;
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const IV: &str = "202122232425262728292a2b2c2d2e2f";
const WRONG_IV: &str = "303132333435363738393a3b3c3d3e3f";
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
    .with_decode(true)
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
            original.replace(
                &format!("value=\"{}\"", target.as_str()),
                &format!(
                    "value=\"{}\"",
                    if target == TargetProtection::Cenc {
                        "cbcs"
                    } else {
                        "cenc"
                    }
                ),
            ),
            original.replace(
                "00010203-0405-0607-0809-0a0b0c0d0e0f",
                "10010203-0405-0607-0809-0a0b0c0d0e0f",
            ),
            original.replace("urn:mpeg:cenc:2013", "urn:example:wrong-cenc"),
            original.replace("</MPD>", "<cenc:pssh>AAAA</cenc:pssh></MPD>"),
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
        reject_provider_bytes(events, output, entrypoint, target, keys);
        verify_referenced_initializations(events, output, entrypoint, target, keys);
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
        let key_line = original
            .lines()
            .find(|line| line.starts_with("#EXT-X-KEY:"))
            .unwrap();
        let delayed_key = format!(
            "{}\n{key_line}\n",
            original.replace(&format!("{key_line}\n"), "")
        );
        for changed in [
            original.replace(URI, "skd://different-private-content"),
            original.replace("KEYFORMATVERSIONS=\"1\"", "KEYFORMATVERSIONS=\"2\""),
            original.replace("METHOD=SAMPLE-AES", "METHOD=AES-128"),
            original.replace(version, &lower),
            original.replace(&format!("{version}\n"), ""),
            original.replace(version, &format!("{version}\n{version}")),
            original.replace(version, "#EXT-X-VERSION:invalid"),
            delayed_key,
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
        if format == OutputFormat::Hls {
            reject_map_scope(events, output, entrypoint, target, keys);
        }
        verify_explicit_iv(events, output, entrypoint, format, target, keys);
    }
}
fn aliased_representation(id: &str, base: &str, init: &str, media: &str) -> String {
    format!(
        "<Representation id=\"{id}\">{base}<SegmentTemplate timescale=\"1\" startNumber=\"1\" initialization=\"{init}\" media=\"{media}\"><SegmentTimeline><S d=\"1\"/></SegmentTimeline></SegmentTemplate></Representation>"
    )
}
fn verify_referenced_initializations(
    events: &Arc<Collector>,
    output: &Path,
    entrypoint: &Path,
    target: TargetProtection,
    keys: &key_file::OperatorKeys,
) {
    let original = std::fs::read_to_string(entrypoint).unwrap();
    let document = roxmltree::Document::parse(&original).unwrap();
    let representation = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "Representation")
        .unwrap();
    let init = std::fs::read(output.join("track-0000/init.mp4")).unwrap();
    let actual = output.join("actual");
    assert!(!actual.exists());
    drmr_core::native::create_media_dir_all(&actual).unwrap();
    for path in [
        actual.join("good.mp4"),
        actual.join("good-A.mp4"),
        actual.join("a-1.m4s"),
        output.join("good.mp4"),
        output.join("other.mp4"),
        output.join("other-B.mp4"),
    ] {
        assert!(!path.exists());
        std::fs::write(path, &init).unwrap();
    }
    let first = aliased_representation(
        "A",
        "<BaseURL>actual/</BaseURL>",
        "good.mp4",
        "a-$Number$.m4s",
    );
    let second = aliased_representation("B", "", "other.mp4", "good.mp4");
    let mut changed = original.clone();
    changed.replace_range(representation.range(), &format!("{first}{second}"));
    std::fs::write(entrypoint, &changed).unwrap();
    let mut checks = vec![
        converter(events)
            .verify_output_signaling(entrypoint, OutputFormat::Dash, target, keys)
            .is_ok(),
    ];
    let boxes = drmr_core::mp4::parse_boxes(&init).unwrap();
    let provider = drmr_core::mp4::walk(&boxes)
        .into_iter()
        .find(|node| node.kind() == *b"pssh")
        .unwrap();
    let mut wrong = init.clone();
    wrong[provider.header.offset + provider.header.size - 1] ^= 1;
    std::fs::write(actual.join("good.mp4"), wrong).unwrap();
    checks.push(
        converter(events)
            .verify_output_signaling(entrypoint, OutputFormat::Dash, target, keys)
            .is_err(),
    );
    std::fs::write(actual.join("good.mp4"), &init).unwrap();
    changed = changed
        .replace(
            "initialization=\"good.mp4\"",
            "initialization=\"good-$RepresentationID$.mp4\"",
        )
        .replace(
            "initialization=\"other.mp4\"",
            "initialization=\"other-$RepresentationID$.mp4\"",
        );
    std::fs::write(entrypoint, changed).unwrap();
    checks.push(
        converter(events)
            .verify_output_signaling(entrypoint, OutputFormat::Dash, target, keys)
            .is_ok(),
    );
    std::fs::write(entrypoint, original).unwrap();
    for name in ["good.mp4", "other.mp4", "other-B.mp4"] {
        std::fs::remove_file(output.join(name)).unwrap();
    }
    std::fs::remove_dir_all(actual).unwrap();
    assert_eq!(
        checks, [true; 3],
        "BaseURL and template expansion must select actual init bytes, including when a valid raw-path decoy is in the same graph"
    );
}
fn reject_map_scope(
    events: &Arc<Collector>,
    output: &Path,
    entrypoint: &Path,
    target: TargetProtection,
    keys: &key_file::OperatorKeys,
) {
    let path = output.join("track-0000/media.m3u8");
    let original = std::fs::read_to_string(&path).unwrap();
    let map = original
        .lines()
        .find(|line| line.starts_with("#EXT-X-MAP:"))
        .unwrap();
    let without = original.replace(&format!("{map}\n"), "");
    let mut refused = Vec::new();
    for changed in [
        original.replace(map, &format!("{map},BYTERANGE=\"1@0\"")),
        format!("{without}\n{map}\n"),
        original.replace(map, &format!("{map}\n{map}")),
        without,
    ] {
        assert_ne!(changed, original);
        std::fs::write(&path, changed).unwrap();
        refused.push(
            converter(events)
                .verify_output_signaling(entrypoint, OutputFormat::Hls, target, keys)
                .err()
                .map(|error| error.0),
        );
    }
    std::fs::write(path, original).unwrap();
    assert_eq!(
        refused,
        vec![Some("output-signaling-verification-failed"); 4],
        "Ranges, late maps, duplicate maps, and missing maps cannot select a complete initialization"
    );
}
fn reject_provider_bytes(
    events: &Arc<Collector>,
    output: &Path,
    entrypoint: &Path,
    target: TargetProtection,
    keys: &key_file::OperatorKeys,
) {
    let original = std::fs::read_to_string(entrypoint).unwrap();
    let document = roxmltree::Document::parse(&original).unwrap();
    for node in document
        .descendants()
        .filter(|node| node.is_element() && matches!(node.tag_name().name(), "pssh" | "pro"))
    {
        let range = node.first_child().unwrap().range();
        let mut changed = original.clone();
        changed.replace_range(
            range.start..=range.start,
            if &original[range.start..=range.start] == "A" {
                "B"
            } else {
                "A"
            },
        );
        std::fs::write(entrypoint, changed).unwrap();
        assert!(
            converter(events)
                .verify_output_signaling(entrypoint, OutputFormat::Dash, target, keys)
                .is_err()
        );
    }
    std::fs::write(entrypoint, original).unwrap();
    let path = output.join("track-0000/init.mp4");
    let original = std::fs::read(&path).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&original).unwrap();
    for node in drmr_core::mp4::walk(&boxes)
        .into_iter()
        .filter(|node| node.kind() == *b"pssh")
    {
        let mut changed = original.clone();
        changed[node.header.offset + node.header.size - 1] ^= 1;
        std::fs::write(&path, changed).unwrap();
        assert!(
            converter(events)
                .verify_output_signaling(entrypoint, OutputFormat::Dash, target, keys)
                .is_err()
        );
    }
    std::fs::write(path, original).unwrap();
}
fn verify_explicit_iv(
    events: &Arc<Collector>,
    output: &Path,
    entrypoint: &Path,
    format: OutputFormat,
    target: TargetProtection,
    keys: &key_file::OperatorKeys,
) {
    let path = output.join("track-0000/media.m3u8");
    let original = std::fs::read_to_string(&path).unwrap();
    let iv = if format == OutputFormat::Hls {
        drmr_core::mp4::tenc_constant_iv_hex(
            &std::fs::read(output.join("track-0000/init.mp4")).unwrap(),
        )
        .unwrap()
        .unwrap()
        .expose()
        .to_string()
    } else {
        IV.into()
    };
    let line = original
        .lines()
        .find(|line| line.starts_with("#EXT-X-KEY:"))
        .unwrap();
    assert!(!line.contains(",IV="));
    std::fs::write(&path, original.replace(line, &format!("{line},IV=0x{iv}"))).unwrap();
    converter(events)
        .verify_output_signaling(entrypoint, format, target, keys)
        .unwrap();
    let mut wrong = iv.clone();
    wrong.replace_range(..1, if iv.starts_with('0') { "1" } else { "0" });
    std::fs::write(
        &path,
        original.replace(line, &format!("{line},IV=0x{wrong}")),
    )
    .unwrap();
    assert!(
        converter(events)
            .verify_output_signaling(entrypoint, format, target, keys)
            .is_err()
    );
    let version = original
        .lines()
        .find(|line| line.starts_with("#EXT-X-VERSION:"))
        .unwrap();
    std::fs::write(&path, original.replace(version, "#EXT-X-VERSION:7")).unwrap();
    converter(events)
        .verify_output_signaling(entrypoint, format, target, keys)
        .unwrap();
    std::fs::write(path, original).unwrap();
}
fn reject_runtime_iv(events: &Arc<Collector>, input: &Path, entrypoint: &Path, root: &Path) {
    let wrong = keys(
        root,
        "wrong-iv",
        &serde_json::json!({"key":KEY,"iv":WRONG_IV,"hls_key_uri":URI,"signaling":{"system":"multi-drm","fairplay":true}}),
    );
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let output_hls = wrong.output_hls_verification_keys().unwrap();
    let result = converter(events).verify(VerificationRequest {
        source: VerificationSource {
            input,
            input_keys: &empty,
            hls_keys: &hls,
        },
        output: VerificationSource {
            input: entrypoint,
            input_keys: &empty,
            hls_keys: &output_hls,
        },
    });
    match result {
        Ok(report) => {
            assert!(!report.passed);
            println!("DRMR_WRONG_IV_REJECTED comparison-mismatch");
        }
        Err(error) => {
            assert!(
                matches!(
                    error.0,
                    "media-tool-failed"
                        | "ts-sample-encryption-invalid"
                        | "ts-original-evidence-invalid"
                        | "invalid-ts-access-unit-evidence"
                ),
                "Wrong-IV proof must not pass on unrelated operational failures: {}",
                error.0
            );
            println!("DRMR_WRONG_IV_REJECTED {}", error.0);
        }
    }
}
fn snapshot_source(root: &Path) -> std::collections::BTreeMap<std::ffi::OsString, [u8; 32]> {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.path().symlink_metadata().unwrap();
            assert!(metadata.is_file() && !metadata.is_symlink());
            (
                entry.file_name(),
                sha2::Sha256::digest(std::fs::read(entry.path()).unwrap()).into(),
            )
        })
        .collect()
}
fn output_cases() -> [(
    &'static str,
    OutputFormat,
    TargetProtection,
    serde_json::Value,
); 8] {
    [
        (
            "dash",
            OutputFormat::Dash,
            TargetProtection::Cbcs,
            serde_json::json!({"kid":KID,"key":KEY,"signaling":{"system":"multi-drm","widevine_pssh":pssh(),"playready_license_url":LICENSE}}),
        ),
        (
            "dash-cenc",
            OutputFormat::Dash,
            TargetProtection::Cenc,
            serde_json::json!({"kid":KID,"key":KEY,"signaling":{"system":"multi-drm","widevine_pssh":pssh(),"playready_license_url":LICENSE}}),
        ),
        (
            "widevine-cenc",
            OutputFormat::Dash,
            TargetProtection::Cenc,
            serde_json::json!({"kid":KID,"key":KEY,"signaling":{"system":"multi-drm","widevine_pssh":pssh()}}),
        ),
        (
            "playready-cenc",
            OutputFormat::Dash,
            TargetProtection::Cenc,
            serde_json::json!({"kid":KID,"key":KEY,"signaling":{"system":"multi-drm","playready_license_url":LICENSE}}),
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
        (
            "widevine",
            OutputFormat::Dash,
            TargetProtection::Cbcs,
            serde_json::json!({"kid":KID,"key":KEY,"signaling":{"system":"multi-drm","widevine_pssh":pssh()}}),
        ),
        (
            "playready",
            OutputFormat::Dash,
            TargetProtection::Cbcs,
            serde_json::json!({"kid":KID,"key":KEY,"signaling":{"system":"multi-drm","playready_license_url":LICENSE}}),
        ),
    ]
}
fn assert_report(report: &ConversionReport, name: &str) {
    assert!(report.verification.passed);
    assert!(!report.verification.license_playback_verified);
    let expected = match name {
        "dash" | "dash-cenc" => vec!["playready", "widevine"],
        "widevine" | "widevine-cenc" => vec!["widevine"],
        "playready" | "playready-cenc" => vec!["playready"],
        _ => vec!["fairplay"],
    };
    assert_eq!(
        report.output.drm_systems, expected,
        "The report must describe actual final DRM declarations"
    );
}
fn assert_unspecified_hls_language(entrypoint: &Path) {
    let text = std::fs::read_to_string(entrypoint).unwrap();
    for line in text
        .lines()
        .filter_map(|line| line.strip_prefix("#EXT-X-MEDIA:"))
    {
        let attrs = drmr_core::m3u8::parse_attributes(line).unwrap();
        assert!(
            attrs.get("LANGUAGE").is_none(),
            "An unspecified source language must remain unspecified in HLS"
        );
    }
}
#[test]
fn clear_transport_has_independently_verified_dash_and_fairplay_outputs() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let input = source(&owner.path().join("source"));
    let original = snapshot_source(input.parent().unwrap());
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
        assert_report(&report, name);
        let entrypoint = output.join(if format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        });
        if format == OutputFormat::Dash {
            assert_label_order(&entrypoint);
        } else {
            assert_unspecified_hls_language(&entrypoint);
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
        assert!(verified.decode_evidence.satisfies(
            drmr_core::conversion::ValidationDepth::Decoded,
            verified.scope
        ));
        assert!(
            converter(&events)
                .verify_output_signaling(
                    &entrypoint,
                    format,
                    target,
                    &key_file::OperatorKeys::default()
                )
                .is_err(),
            "Unrequested provider records must be rejected"
        );
        if format == OutputFormat::HlsTs {
            reject_runtime_iv(&events, &input, &entrypoint, owner.path());
        }
        reject_mutations(&events, &output, &entrypoint, format, target, &keys);
        converter(&events)
            .verify_output_signaling(&entrypoint, format, target, &keys)
            .unwrap();
        retain_manifests(&output, name);
        assert_eq!(snapshot_source(input.parent().unwrap()), original);
        println!(
            "DRMR_MULTI_DRM_CELL {}",
            serde_json::json!({"format":format,"protection":target,"systems":keys.signaling.as_ref().unwrap().systems(),"media_verified":true,"independent_signaling_verified":true,"independent_media_verified":true,"packet_payloads_equal":true,"packet_timing_equal":true,"decoded_content_equal":true,"license_playback_verified":false})
        );
    }
    let rendered = format!("{:?}", events.records());
    for private in [KEY, IV, WRONG_IV, LICENSE, URI, pssh().as_str()] {
        assert!(!rendered.contains(private));
    }
    assert!(OutputSignaling::multi_drm(Some(&pssh()), None, false).is_ok());
}
