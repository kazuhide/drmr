//! Original header admission must precede any normalization into private playlists.
use super::*;
use drmr_core::{
    conversion::{Aes128Key, ConversionRequest, HlsOutputKey, plan::ExecutionPlanRequest},
    events::Collector,
};

fn original_with_signed_target(path: &Path) {
    let text = fs::read_to_string(path).unwrap();
    let target = text
        .lines()
        .find(|line| line.starts_with("#EXT-X-TARGETDURATION:"))
        .unwrap();
    let value = target.split_once(':').unwrap().1;
    fs::write(
        path,
        text.replace(target, &format!("#EXT-X-TARGETDURATION:+{value}")),
    )
    .unwrap();
}

fn request<'a>(
    input: &'a Path,
    output: &'a Path,
    format: OutputFormat,
    keys: &'a InputKeys,
    hls_keys: &'a HlsInputKeys,
) -> ConversionRequest<'a> {
    ConversionRequest {
        input,
        output,
        format,
        target: TargetProtection::Clear,
        input_keys: keys,
        hls_input_keys: hls_keys,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    }
}

fn saved_refusals(command: &Converter, input: &Path, root: &Path) {
    let keys = InputKeys::default();
    let hls_keys = HlsInputKeys::default();
    let source = VerificationSource {
        input,
        input_keys: &keys,
        hls_keys: &hls_keys,
    };
    let single = root.join("saved-single");
    assert_eq!(
        command
            .create_execution_plan(ExecutionPlanRequest {
                input: source,
                output: &single,
                format: Some(OutputFormat::Hls),
                target: TargetProtection::Clear,
            })
            .unwrap_err()
            .0,
        "hls-invalid-target-duration"
    );
    let bundle = root.join("saved-bundle");
    let output = BundleOutputIntent {
        dash_addressing: None,
        id: "hls".into(),
        format: OutputFormat::Hls,
        target: TargetProtection::Clear,
        drms: Vec::new(),
        conformance: ManifestValidation::None,
        processing: None,
    };
    assert_eq!(
        command
            .create_bundle_plan(BundlePlanRequest {
                input: source,
                output_root: &bundle,
                outputs: &[output]
            })
            .unwrap_err()
            .0,
        "hls-invalid-target-duration"
    );
    assert!(!single.exists() && !bundle.exists());
}

fn observed_converter(observed: &Arc<Collector>, decoded: bool) -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), observed.clone())),
        "packager",
        "a-decoder-that-must-not-run",
    )
    .with_ffprobe("a-probe-that-must-not-run")
    .with_processing(ProcessingMode::Stitch)
    .with_decode(decoded)
}

fn direct_refusals(input: &Path, root: &Path, transport: bool, aes: &HlsOutputKey) {
    let keys = InputKeys::default();
    let hls_keys = HlsInputKeys::default();
    let observed = Arc::new(Collector::default());
    let command = observed_converter(&observed, false);
    for decoded in [false, true] {
        for format in [OutputFormat::Hls, OutputFormat::Dash] {
            let output = root.join(format!("direct-{decoded}-{format:?}"));
            assert_eq!(
                observed_converter(&observed, decoded)
                    .convert(request(input, &output, format, &keys, &hls_keys))
                    .unwrap_err()
                    .0,
                "hls-invalid-target-duration"
            );
            assert!(!output.exists());
        }
    }
    if transport {
        let output = root.join("direct-ts");
        assert_eq!(
            command
                .convert(request(
                    input,
                    &output,
                    OutputFormat::HlsTs,
                    &keys,
                    &hls_keys
                ))
                .unwrap_err()
                .0,
            "hls-invalid-target-duration"
        );
        assert!(!output.exists());
    }
    let output = root.join("direct-aes");
    let mut aes_request = request(input, &output, OutputFormat::Hls, &keys, &hls_keys);
    aes_request.target = TargetProtection::HlsAes128;
    aes_request.hls_output_key = Some(aes);
    assert_eq!(
        command.convert(aes_request).unwrap_err().0,
        "hls-invalid-target-duration"
    );
    assert!(!output.exists());
    if !transport {
        let output = root.join("legacy-aes");
        assert_eq!(
            command
                .convert_hls_to_hls_aes128(input, &output, &keys, &hls_keys, aes)
                .unwrap_err()
                .0,
            "hls-invalid-target-duration"
        );
        assert!(!output.exists());
    }
    saved_refusals(&command, input, root);
    assert!(
        observed
            .records()
            .iter()
            .all(
                |record| record.get("event").and_then(serde_json::Value::as_str) != Some("command")
            )
    );
}

#[test]
fn original_signed_hls_target_is_refused_by_direct_and_saved_routes_before_tools() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("source"), 44_100);
    let fmp4 = root.path().join("fmp4");
    let keys = InputKeys::default();
    let hls_keys = HlsInputKeys::default();
    assert!(
        converter()
            .convert(request(&input, &fmp4, OutputFormat::Hls, &keys, &hls_keys))
            .unwrap()
            .verification
            .passed
    );
    let master = fs::read_to_string(fmp4.join("master.m3u8")).unwrap();
    let video = fmp4.join(
        master
            .lines()
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .unwrap(),
    );
    let aes = HlsOutputKey::new(
        "https://keys.example.invalid/target-duration",
        Aes128Key::from_hex("101112131415161718191a1b1c1d1e1f").unwrap(),
    )
    .unwrap();
    let control = root.path().join("qualified-legacy-aes");
    assert!(
        converter()
            .convert_hls_to_hls_aes128(&video, &control, &keys, &hls_keys, &aes)
            .unwrap()
            .verification
            .passed
    );
    for (index, (path, transport)) in [(&input, true), (&video, false)].into_iter().enumerate() {
        original_with_signed_target(path);
        let original = common::snapshot_tree(path.parent().unwrap());
        let destinations = root.path().join(format!("attempts-{index}"));
        fs::create_dir(&destinations).unwrap();
        direct_refusals(path, &destinations, transport, &aes);
        assert_eq!(common::snapshot_tree(path.parent().unwrap()), original);
    }
}
