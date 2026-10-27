//! Public saved plans retain complete tool capabilities and inspect decrypted output metadata.
use super::*;
use crate::conversion::{
    conformance::ManifestValidationTools,
    dash_addressing::DashAddressing,
    key_file::{BundleOperatorKeys, OperatorKeys},
    output_options::OutputOptions,
    plan::{
        self, SavedPlan,
        bundle::{
            BundleOutputIntent, BundlePlan, BundlePlanRequest, ManifestValidation, OutputSettings,
        },
    },
};
use std::fs;

fn intent(
    id: &str,
    format: OutputFormat,
    target: TargetProtection,
    processing: ProcessingMode,
) -> BundleOutputIntent {
    BundleOutputIntent {
        dash_addressing: None,
        id: id.into(),
        format,
        target,
        drms: Vec::new(),
        conformance: ManifestValidation::None,
        processing: Some(processing),
    }
}

fn assert_bound_output_options(saved: &BundlePlan) {
    assert_eq!(saved.schema_version, 10);
    assert_eq!(
        saved.output_options,
        Some(vec![
            OutputSettings {
                id: "dash".into(),
                options: OutputOptions {
                    layout: None,
                    dash_addressing: Some(DashAddressing::Template),
                },
            },
            OutputSettings {
                id: "mp4".into(),
                options: OutputOptions::default(),
            },
        ])
    );
}

#[test]
fn review_mixed_single_video_bundle_keeps_mp4_stitch_eligibility_and_segment_refusals() {
    let mut fixture = fixture(48_000);
    fixture.parts.truncate(1);
    let input = manifest_fixture(
        &fixture,
        TargetProtection::Clear,
        OutputFormat::Hls,
        "source",
    );
    let (base, _) = converter();
    let command = Converter::new(base.backend.runner.clone(), "packager", "ffmpeg");
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let source = VerificationSource {
        input: &input,
        input_keys: &empty,
        hls_keys: &hls,
    };
    let qualified = command
        .with_processing_clone(ProcessingMode::Segments)
        .validate_source(source, SourceValidationPurpose::Conversion)
        .unwrap();
    assert!(qualified.eligible_formats.contains(&OutputFormat::Mp4));
    let output = fixture.directory.path().join("bundle");
    let outputs = [
        intent(
            "dash",
            OutputFormat::Dash,
            TargetProtection::Clear,
            ProcessingMode::Segments,
        ),
        intent(
            "mp4",
            OutputFormat::Mp4,
            TargetProtection::Clear,
            ProcessingMode::Stitch,
        ),
    ];
    let saved = command
        .create_bundle_plan(BundlePlanRequest {
            input: source,
            output_root: &output,
            outputs: &outputs,
        })
        .unwrap();
    assert_bound_output_options(&saved);
    let plan_path = fixture.directory.path().join("mixed.plan.json");
    plan::write_bundle(&plan_path, &saved).unwrap();
    let SavedPlan::Bundle(loaded) = plan::read_saved(&plan_path).unwrap() else {
        panic!("saved bundle must reopen");
    };
    assert_eq!(loaded.output_options, saved.output_options);
    let keys = BundleOperatorKeys::default();
    let tools = ManifestValidationTools::default();
    let converted = command.execute_bundle_plan(&loaded, &keys, &tools).unwrap();
    assert!(
        converted
            .outputs
            .iter()
            .all(|output| output.conversion.verification.passed)
    );
    assert_eq!(converted.outputs.len(), 2);
    let verified = command.verify_bundle_plan(&loaded, &keys, &tools).unwrap();
    assert!(verified.passed && verified.source_unchanged);
    assert!(
        verified
            .outputs
            .iter()
            .all(|output| output.output_matches_plan && output.verification.passed)
    );
    let (forced, events) = converter();
    let refused = fixture.directory.path().join("forced.mp4");
    let error = forced
        .with_processing(ProcessingMode::Segments)
        .convert(ConversionRequest {
            input: &input,
            output: &refused,
            format: OutputFormat::Mp4,
            target: TargetProtection::Clear,
            input_keys: &empty,
            hls_input_keys: &hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap_err();
    assert_eq!(error.0, "processing-container-change-not-supported");
    assert!(!refused.exists());
    assert_no_tools(&events);
}

struct AesCase {
    fixture: Fixture,
    input: PathBuf,
    output: PathBuf,
    plan: BundlePlan,
    keys: BundleOperatorKeys,
    command: Converter,
    events: Arc<Collector>,
}

fn aes_case() -> AesCase {
    let fixture = fixture(48_000);
    let input = manifest_fixture(
        &fixture,
        TargetProtection::Clear,
        OutputFormat::Hls,
        "source",
    );
    let output = fixture.directory.path().join("bundle");
    let (command, events) = converter();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let outputs = [intent(
        "aes",
        OutputFormat::Hls,
        TargetProtection::HlsAes128,
        ProcessingMode::Segments,
    )];
    let plan = command
        .create_bundle_plan(BundlePlanRequest {
            input: VerificationSource {
                input: &input,
                input_keys: &empty,
                hls_keys: &hls,
            },
            output_root: &output,
            outputs: &outputs,
        })
        .unwrap();
    let mut keys = BundleOperatorKeys::default();
    let mut child = OperatorKeys::default();
    child.hls_output = Some(
        crate::conversion::HlsOutputKey::new(
            "https://example.invalid/key",
            crate::conversion::Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
        )
        .unwrap(),
    );
    keys.outputs.insert("aes".into(), child);
    command
        .execute_bundle_plan(&plan, &keys, &ManifestValidationTools::default())
        .unwrap();
    assert!(
        command
            .verify_bundle_plan(&plan, &keys, &ManifestValidationTools::default())
            .unwrap()
            .passed
    );
    AesCase {
        fixture,
        input,
        output,
        plan,
        keys,
        command,
        events,
    }
}

fn resource_iv(playlist: &str, file: &str) -> crate::conversion::SegmentIv {
    let mut iv = None;
    for line in playlist.lines() {
        if let Some(value) = line.strip_prefix("#EXT-X-KEY:") {
            let attrs = crate::conversion::hls::attributes(value).unwrap();
            assert_eq!(attrs.get("METHOD"), Some("AES-128"));
            iv = attrs.get("IV").map(str::to_owned);
        }
        let selected = if let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
            crate::conversion::hls::attributes(value)
                .unwrap()
                .get("URI")
                == Some(file)
        } else {
            line == file
        };
        if selected {
            return crate::conversion::SegmentIv::from_hex(iv.as_ref().unwrap()).unwrap();
        }
    }
    panic!("fixture resource must have an explicit IV");
}

fn inject_encrypted_provider(case: &AesCase, file: &str) {
    let root = case.output.join("aes/track-0000");
    let playlist = fs::read_to_string(root.join("media.m3u8")).unwrap();
    let iv = resource_iv(&playlist, file);
    let key = case.keys.outputs["aes"].hls_output.as_ref().unwrap();
    let clear = case.fixture.directory.path().join("modified-plaintext");
    crate::conversion::decrypt_segment(&root.join(file), &clear, &key.key, &iv).unwrap();
    let mut payload = vec![0x12, 0x10];
    payload.extend(hex::decode(SOURCE_ID).unwrap());
    let pssh =
        crate::playready::build_pssh_box(&crate::playready::WIDEVINE_SYSTEM_ID, &payload).unwrap();
    let mut bytes = fs::read(&clear).unwrap();
    bytes.extend(pssh);
    fs::write(&clear, bytes).unwrap();
    let encrypted = case.fixture.directory.path().join("modified-ciphertext");
    crate::conversion::encrypt_segment(&clear, &encrypted, &key.key, &iv).unwrap();
    fs::write(root.join(file), fs::read(encrypted).unwrap()).unwrap();
}

#[test]
fn review_fresh_aes_bundle_verification_rejects_unrequested_provider_in_plaintext() {
    for file in ["init.mp4", "seg_1.m4s"] {
        let case = aes_case();
        inject_encrypted_provider(&case, file);
        let error = case
            .command
            .verify_bundle_plan(&case.plan, &case.keys, &ManifestValidationTools::default())
            .unwrap_err();
        assert_eq!(error.0, "output-signaling-verification-failed");
        assert_no_tools(&case.events);
    }
}

#[test]
fn review_encrypted_source_provider_records_remain_supported_inputs() {
    let case = aes_case();
    inject_encrypted_provider(&case, "init.mp4");
    let input = case.output.join("aes/master.m3u8");
    let output = case.fixture.directory.path().join("clear-output");
    let keys = case.keys.outputs["aes"]
        .output_hls_verification_keys()
        .unwrap();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let command = case.command.with_processing_clone(ProcessingMode::Segments);
    let report = command
        .convert(ConversionRequest {
            input: &input,
            output: &output,
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            input_keys: &empty,
            hls_input_keys: &keys,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap();
    assert!(report.verification.passed);
    let result = output.join("manifest.mpd");
    assert!(
        command
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &case.input,
                    input_keys: &empty,
                    hls_keys: &hls
                },
                output: VerificationSource {
                    input: &result,
                    input_keys: &empty,
                    hls_keys: &hls
                }
            })
            .unwrap()
            .passed
    );
    assert_no_tools(&case.events);
}

#[test]
fn review_plaintext_metadata_guard_rejects_a_later_resource_change() {
    use std::io::Write as _;
    let case = aes_case();
    let key = &case.keys.outputs["aes"];
    let keys = key.output_verification_keys().unwrap();
    let hls = key.output_hls_verification_keys().unwrap();
    let output = case.output.join("aes/master.m3u8");
    let source = super::super::source::Source::load(
        &case.command,
        VerificationSource {
            input: &output,
            input_keys: &keys,
            hls_keys: &hls,
        },
    )
    .unwrap();
    let presentation = source.presentation(&case.command).unwrap();
    let media = super::super::proof::admit(&case.command, &presentation, &keys).unwrap();
    let guard =
        super::super::signaling::verify_plaintext(&case.command, &presentation, &media).unwrap();
    guard.check().unwrap();
    let path = &presentation.parts[0].resources[0].path;
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(&[0, 0, 0, 8, b'f', b'r', b'e', b'e'])
        .unwrap();
    source.check_original().unwrap();
    assert!(guard.check().is_err());
    assert_no_tools(&case.events);
}
