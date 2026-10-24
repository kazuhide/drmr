//! Fresh verification checks the exact output DRM selection in every referenced resource.
use super::*;
use crate::conversion::{
    DrmSystem, OutputSignaling,
    conformance::ManifestValidationTools,
    key_file::{BundleOperatorKeys, OperatorKeys},
    plan::{
        ExecutionPlanRequest,
        bundle::{BundleOutputIntent, BundlePlanRequest, ManifestValidation},
    },
};
use std::{fs, io::Write as _};

fn widevine(kid: &str) -> Vec<u8> {
    let mut payload = vec![0x12, 0x10];
    payload.extend(hex::decode(kid).unwrap());
    crate::playready::build_pssh_box(&crate::playready::WIDEVINE_SYSTEM_ID, &payload).unwrap()
}

fn playready(kid: &str) -> Vec<u8> {
    crate::playready::build_playready_v43(kid, "https://example.invalid/license")
        .unwrap()
        .pssh_box
}

fn append(path: &Path, bytes: &[u8]) {
    fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

#[test]
fn final_single_default_selection_checks_initialization_and_middle_and_final_media() {
    let fixture = fixture(48_000);
    let input = manifest_fixture(
        &fixture,
        TargetProtection::Clear,
        OutputFormat::Hls,
        "source",
    );
    let (command, events) = converter();
    let command = command.with_processing(ProcessingMode::Segments);
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    for format in [OutputFormat::Dash, OutputFormat::Hls] {
        for target in [
            TargetProtection::Clear,
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
        ] {
            if format == OutputFormat::Hls && target == TargetProtection::Cenc {
                continue;
            }
            let output = fixture
                .directory
                .path()
                .join(format!("single-{format:?}-{target:?}"));
            let plan = command
                .create_execution_plan(ExecutionPlanRequest {
                    input: VerificationSource {
                        input: &input,
                        input_keys: &empty,
                        hls_keys: &hls,
                    },
                    output: &output,
                    format: Some(format),
                    target,
                })
                .unwrap();
            assert_eq!(plan.schema_version, 5);
            let mut keys = OperatorKeys::default();
            keys.output = (target != TargetProtection::Clear)
                .then(|| OutputKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap());
            if format == OutputFormat::Hls && target == TargetProtection::Cbcs {
                let path = fixture.directory.path().join("identity.keys.json");
                crate::native::create_private_new(&path).unwrap().write_all(
                    serde_json::to_string(&serde_json::json!({"schema_version":1,"output":{
                        "kid":OUTPUT_ID,"key":OUTPUT_KEY,"hls_key_uri":"https://example.invalid/key"
                    }})).unwrap().as_bytes()
                ).unwrap();
                keys = crate::conversion::key_file::read(&path).unwrap();
            }
            assert!(
                command
                    .execute_plan(&plan, &keys)
                    .unwrap()
                    .verification
                    .passed
            );
            let baseline = command.verify_plan(&plan, &keys).unwrap();
            assert!(baseline.output_matches_plan && baseline.verification.passed);
            for name in ["init.mp4", "seg_2.m4s", "seg_3.m4s"] {
                let path = output.join("track-0000").join(name);
                let original = fs::read(&path).unwrap();
                append(&path, &widevine(OUTPUT_ID));
                let result = command.verify_plan(&plan, &keys);
                fs::write(&path, &original).unwrap();
                assert_eq!(
                    result.err(),
                    Some(ConversionError("output-signaling-verification-failed")),
                    "{format:?}/{target:?}/{name}"
                );
            }
        }
    }
    assert_no_tools(&events);
}

#[test]
fn final_widevine_bundle_checks_selected_and_foreign_records_in_every_media_resource() {
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
    let outputs = [BundleOutputIntent {
        id: "wv".into(),
        format: OutputFormat::Dash,
        target: TargetProtection::Cbcs,
        drms: vec![DrmSystem::Widevine],
        conformance: ManifestValidation::None,
        processing: Some(ProcessingMode::Segments),
    }];
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
    let selected = widevine(OUTPUT_ID);
    let mut keys = BundleOperatorKeys::default();
    let mut child = OperatorKeys::default();
    child.output = Some(OutputKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap());
    child.signaling =
        Some(OutputSignaling::multi_drm(Some(&hex::encode(&selected)), None, false).unwrap());
    keys.outputs.insert("wv".into(), child);
    let tools = ManifestValidationTools::default();
    command.execute_bundle_plan(&plan, &keys, &tools).unwrap();
    assert!(
        command
            .verify_bundle_plan(&plan, &keys, &tools)
            .unwrap()
            .passed
    );
    for part in ["track-0000", "track-0001"] {
        for name in ["seg_2.m4s", "seg_3.m4s"] {
            let path = output.join("wv").join(part).join(name);
            let original = fs::read(&path).unwrap();
            append(&path, &selected);
            assert!(
                command
                    .verify_bundle_plan(&plan, &keys, &tools)
                    .unwrap()
                    .passed,
                "the selected exact provider record is valid in media"
            );
            for record in [playready(OUTPUT_ID), widevine(SOURCE_ID), selected.clone()] {
                append(&path, &record);
                let result = command.verify_bundle_plan(&plan, &keys, &tools);
                fs::write(&path, [&original[..], &selected].concat()).unwrap();
                assert_eq!(
                    result.err(),
                    Some(ConversionError("output-signaling-verification-failed")),
                    "{part}/{name}"
                );
            }
            fs::write(path, original).unwrap();
        }
    }
    assert_no_tools(&events);
}

#[test]
fn final_fairplay_bundle_rejects_provider_records_in_middle_and_final_media() {
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
    let outputs = [BundleOutputIntent {
        id: "fp".into(),
        format: OutputFormat::Hls,
        target: TargetProtection::Cbcs,
        drms: vec![DrmSystem::FairPlay],
        conformance: ManifestValidation::None,
        processing: Some(ProcessingMode::Segments),
    }];
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
    let key_file = fixture.directory.path().join("fairplay.keys.json");
    crate::native::create_private_new(&key_file).unwrap().write_all(serde_json::to_string(&serde_json::json!({"schema_version":1,"output":{"kid":OUTPUT_ID,"key":OUTPUT_KEY,"hls_key_uri":"skd://synthetic/fairplay","signaling":{"system":"multi-drm","fairplay":true}}})).unwrap().as_bytes()).unwrap();
    keys.outputs.insert(
        "fp".into(),
        crate::conversion::key_file::read(&key_file).unwrap(),
    );
    let tools = ManifestValidationTools::default();
    command.execute_bundle_plan(&plan, &keys, &tools).unwrap();
    assert!(
        command
            .verify_bundle_plan(&plan, &keys, &tools)
            .unwrap()
            .passed
    );
    for part in ["track-0000", "track-0001"] {
        for name in ["init.mp4", "seg_2.m4s", "seg_3.m4s"] {
            let path = output.join("fp").join(part).join(name);
            let original = fs::read(&path).unwrap();
            append(&path, &widevine(OUTPUT_ID));
            let result = command.verify_bundle_plan(&plan, &keys, &tools);
            fs::write(&path, original).unwrap();
            assert_eq!(
                result.err(),
                Some(ConversionError("output-signaling-verification-failed")),
                "{part}/{name}"
            );
        }
    }
    assert_no_tools(&events);
}

#[test]
fn final_encrypted_source_media_provider_records_are_replaced_without_changing_media() {
    let fixture = fixture(48_000);
    let reference = manifest_fixture(
        &fixture,
        TargetProtection::Clear,
        OutputFormat::Hls,
        "reference",
    );
    let (command, events) = converter();
    let command = command.with_processing(ProcessingMode::Segments);
    let keys = InputKeys::new(vec![
        crate::conversion::ContentKey::from_hex(SOURCE_ID, SOURCE_KEY).unwrap(),
    ])
    .unwrap();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let output_key = OutputKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap();
    for format in [OutputFormat::Hls, OutputFormat::Dash] {
        let input = manifest_fixture(
            &fixture,
            TargetProtection::Cbcs,
            format,
            &format!("source-{format:?}"),
        );
        let root = input.parent().unwrap();
        for part in ["track-0000", "track-0001"] {
            for name in ["init.mp4", "seg_2.m4s", "seg_3.m4s"] {
                append(&root.join(part).join(name), &playready(SOURCE_ID));
            }
        }
        for target in [TargetProtection::Clear, TargetProtection::Cbcs] {
            let output = fixture
                .directory
                .path()
                .join(format!("replacement-{format:?}-{target:?}"));
            let converted = command
                .convert(ConversionRequest {
                    input: &input,
                    output: &output,
                    format: OutputFormat::Dash,
                    target,
                    input_keys: &keys,
                    hls_input_keys: &hls,
                    output_key: (target != TargetProtection::Clear).then_some(&output_key),
                    hls_key_uri: None,
                    hls_output_key: None,
                })
                .unwrap();
            assert!(converted.verification.passed);
            let verification_keys = if target == TargetProtection::Clear {
                InputKeys::default()
            } else {
                InputKeys::new(vec![
                    crate::conversion::ContentKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap(),
                ])
                .unwrap()
            };
            assert!(
                command
                    .verify(VerificationRequest {
                        source: VerificationSource {
                            input: &reference,
                            input_keys: &empty,
                            hls_keys: &hls
                        },
                        output: VerificationSource {
                            input: &output.join("manifest.mpd"),
                            input_keys: &verification_keys,
                            hls_keys: &hls
                        },
                    })
                    .unwrap()
                    .passed
            );
            let mut operator = OperatorKeys::default();
            operator.output = (target != TargetProtection::Clear)
                .then(|| OutputKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap());
            command
                .verify_output_signaling(
                    &output.join("manifest.mpd"),
                    OutputFormat::Dash,
                    target,
                    &operator,
                )
                .unwrap();
        }
    }
    assert_no_tools(&events);
}
