use super::*;

#[test]
fn whole_resource_aes_signaling_and_source_guards_complete_with_one_cpu_token() {
    let fixture = fixture(48_000);
    let input = manifest_fixture(
        &fixture,
        TargetProtection::Clear,
        OutputFormat::Hls,
        "source",
    );
    let events = Arc::new(Collector::default());
    let runner = Arc::new(
        ToolRunner::new(Arc::new(Secrets::new()), events.clone()).with_limiter(Arc::new(
            crate::resources::Limiter::new(1, 512 * 1024 * 1024),
        )),
    );
    let converter = Converter::new(runner, "must-not-run-packager", "must-not-run-ffmpeg")
        .with_timeout(std::time::Duration::from_secs(2));
    let original = converter.capture_source(&input).unwrap();
    let output = fixture.directory.path().join("single-token");
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let key = crate::conversion::HlsOutputKey::new(
        "https://example.invalid/key",
        crate::conversion::Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
    )
    .unwrap();
    let source = super::super::super::source::HlsSource::load(
        &converter,
        VerificationSource {
            input: &input,
            input_keys: &empty,
            hls_keys: &hls,
        },
    )
    .unwrap();
    let report = convert_hls(
        &converter,
        ConversionRequest {
            input: &input,
            output: &output,
            format: OutputFormat::Hls,
            target: TargetProtection::HlsAes128,
            input_keys: &empty,
            hls_input_keys: &hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: Some(&key),
        },
        source,
    )
    .unwrap();
    assert!(report.verification.passed);
    assert!(output.join("master.m3u8").is_file());
    original.check_original().unwrap();
    assert_no_tools(&events);
}

#[test]
fn rekeyed_dash_replaces_provider_records_using_the_generated_output_key() {
    for source in [TargetProtection::Cenc, TargetProtection::Cbcs] {
        for target in [TargetProtection::Cenc, TargetProtection::Cbcs] {
            rekeyed_dash(source, target);
        }
    }
}

fn rekeyed_dash(source_protection: TargetProtection, target: TargetProtection) {
    let fixture = fixture(48_000);
    let input = manifest_fixture(&fixture, source_protection, OutputFormat::Dash, "source");
    let keys = InputKeys::new(vec![
        crate::conversion::ContentKey::from_hex(SOURCE_ID, SOURCE_KEY).unwrap(),
    ])
    .unwrap();
    let key = OutputKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap();
    let mut payload = vec![0x12, 0x10];
    payload.extend(hex::decode(OUTPUT_ID).unwrap());
    let widevine =
        crate::playready::build_pssh_box(&crate::playready::WIDEVINE_SYSTEM_ID, &payload).unwrap();
    let widevine_hex = hex::encode(&widevine);
    let license = "https://example.invalid/license?synthetic=private";
    let signaling =
        crate::conversion::OutputSignaling::multi_drm(Some(&widevine_hex), Some(license), false)
            .unwrap();
    let (converter, events) = converter();
    let converter = converter.with_output_signaling(signaling);
    let output = fixture.directory.path().join("multi-drm");
    let hls = HlsInputKeys::default();
    let report = convert_dash(
        &converter,
        ConversionRequest {
            input: &input,
            output: &output,
            format: OutputFormat::Dash,
            target,
            input_keys: &keys,
            hls_input_keys: &hls,
            output_key: Some(&key),
            hls_key_uri: None,
            hls_output_key: None,
        },
    )
    .unwrap();
    assert!(report.verification.passed);
    let result = output.join("manifest.mpd");
    let frozen =
        crate::conversion::FrozenSource::capture(&result, &converter.backend.stop).unwrap();
    converter
        .verify_segment_signaling(&frozen, OutputFormat::Dash, target, Some(&key), None, None)
        .unwrap();
    let text = std::fs::read_to_string(&result).unwrap();
    assert_eq!(text.matches("<ContentProtection").count(), 6);
    for ordinal in 0..2 {
        let initialization = output.join(format!("track-{ordinal:04}/init.mp4"));
        let bytes = std::fs::read(&initialization).unwrap();
        let nodes = crate::mp4::parse_boxes(&bytes).unwrap();
        let boxes = crate::mp4::walk(&nodes);
        let pssh = boxes
            .iter()
            .filter(|node| node.kind() == *b"pssh")
            .collect::<Vec<_>>();
        assert_eq!(pssh.len(), 2);
        assert!(
            pssh.iter()
                .any(|node| node.header.bytes(&bytes) == widevine)
        );
        assert!(pssh.iter().all(|node| {
            node.header.payload(&bytes).get(4..20)
                != Some(
                    hex::decode("1077efecc0b24d02ace33c1e52e2fb4b")
                        .unwrap()
                        .as_slice(),
                )
        }));
        for node in boxes.iter().filter(|node| node.kind() == *b"tenc") {
            let tenc = crate::mp4::Tenc::parse(node.header.payload(&bytes)).unwrap();
            assert_eq!(hex::encode(tenc.default_kid), OUTPUT_ID);
        }
        let scheme = boxes.iter().find(|node| node.kind() == *b"schm").unwrap();
        assert_eq!(
            scheme.header.payload(&bytes).get(4..8),
            Some(target.as_str().as_bytes())
        );
        reject_init_scheme(
            &converter,
            &result,
            &initialization,
            &bytes,
            scheme.header.offset + scheme.header.header_size + 4,
            target,
            &key,
        );
    }
    let logs = serde_json::to_string(&events.records()).unwrap();
    let report = serde_json::to_string(&report).unwrap();
    for sensitive in [SOURCE_KEY, OUTPUT_KEY, license, widevine_hex.as_str()] {
        assert!(!logs.contains(sensitive));
        assert!(!report.contains(sensitive));
    }
    assert_no_tools(&events);
}

fn reject_init_scheme(
    converter: &Converter,
    result: &std::path::Path,
    initialization: &std::path::Path,
    bytes: &[u8],
    offset: usize,
    target: TargetProtection,
    key: &OutputKey,
) {
    let mut changed = bytes.to_vec();
    changed[offset..offset + 4].copy_from_slice(if target == TargetProtection::Cenc {
        b"cbcs"
    } else {
        b"cenc"
    });
    std::fs::write(initialization, changed).unwrap();
    let fresh = crate::conversion::FrozenSource::capture(result, &converter.backend.stop).unwrap();
    assert_eq!(
        converter.verify_segment_signaling(
            &fresh,
            OutputFormat::Dash,
            target,
            Some(key),
            None,
            None
        ),
        Err(ConversionError("output-signaling-verification-failed"))
    );
    std::fs::write(initialization, bytes).unwrap();
}

#[test]
fn fairplay_hls_uses_final_cbcs_metadata_without_stale_pssh() {
    let fixture = fixture(48_000);
    let input = manifest_fixture(
        &fixture,
        TargetProtection::Cenc,
        OutputFormat::Hls,
        "source",
    );
    let keys = InputKeys::new(vec![
        crate::conversion::ContentKey::from_hex(SOURCE_ID, SOURCE_KEY).unwrap(),
    ])
    .unwrap();
    let key = OutputKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap();
    let uri = "skd://synthetic/fairplay";
    let signaling = crate::conversion::OutputSignaling::multi_drm(None, None, true).unwrap();
    let (converter, events) = converter();
    let converter = converter.with_output_signaling(signaling);
    let hls = HlsInputKeys::default();
    let output = fixture.directory.path().join("fairplay");
    let source = super::super::super::source::HlsSource::load(
        &converter,
        VerificationSource {
            input: &input,
            input_keys: &keys,
            hls_keys: &hls,
        },
    )
    .unwrap();
    let report = convert_hls(
        &converter,
        ConversionRequest {
            input: &input,
            output: &output,
            format: OutputFormat::Hls,
            target: TargetProtection::Cbcs,
            input_keys: &keys,
            hls_input_keys: &hls,
            output_key: Some(&key),
            hls_key_uri: Some(uri),
            hls_output_key: None,
        },
        source,
    )
    .unwrap();
    assert!(report.verification.passed);
    assert!(!report.verification.license_playback_verified);
    let frozen = crate::conversion::FrozenSource::capture(
        &output.join("master.m3u8"),
        &converter.backend.stop,
    )
    .unwrap();
    converter
        .verify_segment_signaling(
            &frozen,
            OutputFormat::Hls,
            TargetProtection::Cbcs,
            Some(&key),
            Some(uri),
            None,
        )
        .unwrap();
    for ordinal in 0..2 {
        let playlist =
            std::fs::read_to_string(output.join(format!("track-{ordinal:04}/media.m3u8"))).unwrap();
        assert!(playlist.contains("#EXT-X-VERSION:6\n"));
        assert!(playlist.contains("KEYFORMAT=\"com.apple.streamingkeydelivery\""));
        let bytes = std::fs::read(output.join(format!("track-{ordinal:04}/init.mp4"))).unwrap();
        let nodes = crate::mp4::parse_boxes(&bytes).unwrap();
        assert!(
            !crate::mp4::walk(&nodes)
                .iter()
                .any(|node| node.kind() == *b"pssh")
        );
    }
    assert!(
        !serde_json::to_string(&events.records())
            .unwrap()
            .contains(uri)
    );
    assert!(!serde_json::to_string(&report).unwrap().contains(uri));
    assert_no_tools(&events);
}
