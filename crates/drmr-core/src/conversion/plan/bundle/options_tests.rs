//! Each output's addressing and path map are one strict saved choice.
use super::*;
use crate::conversion::plan::{self, SavedPlan};

fn value(root: &Path) -> serde_json::Value {
    let plan = tests::sample(root);
    let options = plan::options_tests::options(&plan.source);
    let mut value = serde_json::to_value(&plan).unwrap();
    value["schema_version"] = serde_json::json!(8);
    value["required_depth"] = serde_json::json!("native");
    value["processing"] = serde_json::json!([
        {"id":"dash","selection":{"requested":"auto","resolved":"stitch","boundaries":"resegment","segment_duration_ms":2000}},
        {"id":"alternate","selection":{"requested":"auto","resolved":"stitch","boundaries":"resegment","segment_duration_ms":2000}}
    ]);
    value["output_options"] = serde_json::json!([
        {"id":"dash","options":options}, {"id":"alternate","options":options}
    ]);
    value
}

#[test]
fn version_eight_requires_one_identity_bound_setting_per_named_output() {
    let root = crate::native::private_tempdir().unwrap();
    let parsed = serde_json::from_value::<BundlePlan>(value(root.path()));
    assert!(parsed.is_ok(), "version-eight output settings must parse");
    let mut plan = parsed.unwrap();
    plan.plan_id = identity(&plan).unwrap();
    validate(&plan).unwrap();
    let file = root.path().join("source-layout.bundle.json");
    plan::write_bundle(&file, &plan).unwrap();
    assert_eq!(
        plan::read_saved(&file).unwrap(),
        SavedPlan::Bundle(plan.clone())
    );
    let mut edited = serde_json::to_value(&plan).unwrap();
    edited["output_options"][1]["options"]["dash_addressing"] = serde_json::json!("list");
    assert_eq!(
        validate(&serde_json::from_value(edited).unwrap()).unwrap_err(),
        INVALID
    );
    for replacement in [
        serde_json::Value::Null,
        serde_json::json!([]),
        serde_json::json!([{"id":"dash","options":{"dash_addressing":"template"}}]),
    ] {
        let mut invalid = serde_json::to_value(&plan).unwrap();
        invalid["output_options"] = replacement;
        let parsed = serde_json::from_value::<BundlePlan>(invalid);
        assert!(parsed.is_err() || validate(&parsed.unwrap()).is_err());
    }
    let mut swapped = serde_json::to_value(&plan).unwrap();
    swapped["output_options"].as_array_mut().unwrap().reverse();
    let mut swapped: BundlePlan = serde_json::from_value(swapped).unwrap();
    swapped.plan_id = identity(&swapped).unwrap();
    assert_eq!(validate(&swapped).unwrap_err(), INVALID);
}

#[test]
fn recipes_expose_addressing_only_in_version_three_and_only_for_dash() {
    let root = crate::native::private_tempdir().unwrap();
    for version in [1, 2, 3] {
        for format in ["dash", "hls"] {
            for addressing in [serde_json::json!("template"), serde_json::Value::Null] {
                let body = serde_json::json!({"schema_version":version,"outputs":[{"id":"output","format":format,"target":"clear","drms":[],"dash_addressing":addressing}]});
                let file = root.path().join(format!(
                    "recipe-{version}-{format}-{}.json",
                    addressing.is_null()
                ));
                std::fs::write(&file, serde_json::to_vec(&body).unwrap()).unwrap();
                assert_eq!(
                    read_output_spec(&file).is_ok(),
                    version == 3 && format == "dash" && !addressing.is_null()
                );
            }
        }
    }
}

#[test]
fn old_bundle_fields_stay_omitted_and_source_membership_is_checked_before_execution() {
    let root = crate::native::private_tempdir().unwrap();
    let plan = tests::sample(root.path());
    let old_bytes = serde_json::to_vec(&plan).unwrap();
    assert!(
        serde_json::to_value(&plan)
            .unwrap()
            .get("output_options")
            .is_none()
    );
    let reopened: BundlePlan = serde_json::from_slice(&old_bytes).unwrap();
    validate(&reopened).unwrap();
    assert_eq!(serde_json::to_vec(&reopened).unwrap(), old_bytes);
    let mut source_map = value(root.path());
    source_map["output_options"][0]["options"]["layout"]["tracks"][0]["source"] =
        serde_json::json!("unlisted.mp4");
    let parsed = serde_json::from_value::<BundlePlan>(source_map);
    assert!(
        parsed.is_ok(),
        "new schema must parse before checking inventory membership"
    );
    let mut plan = parsed.unwrap();
    plan.plan_id = identity(&plan).unwrap();
    assert_eq!(validate(&plan).unwrap_err(), INVALID);
    assert!(!plan.output_root.exists());
}

#[test]
fn every_legacy_bundle_schema_refuses_new_settings_without_changing_its_bytes() {
    let root = crate::native::private_tempdir().unwrap();
    let mut plan = tests::sample(root.path());
    for version in [2, 4, 6] {
        plan.schema_version = version;
        plan.required_depth = (version >= 4).then_some(ValidationDepth::Native);
        plan.processing = (version == 6).then(|| {
            plan.outputs
                .iter()
                .map(|output| OutputProcessing {
                    id: output.id.clone(),
                    selection: ProcessingSelection::resolve(
                        ProcessingMode::Stitch,
                        plan.source_format,
                        output.format,
                    )
                    .unwrap(),
                })
                .collect()
        });
        plan.plan_id = identity(&plan).unwrap();
        let bytes = serde_json::to_vec(&plan).unwrap();
        let reopened: BundlePlan = serde_json::from_slice(&bytes).unwrap();
        validate(&reopened).unwrap();
        assert_eq!(serde_json::to_vec(&reopened).unwrap(), bytes);
        for new_field in ["output_options", "dash_addressing"] {
            let mut invalid = serde_json::to_value(&plan).unwrap();
            if new_field == "output_options" {
                invalid[new_field] = value(root.path())[new_field].clone();
            } else {
                invalid["outputs"][0][new_field] = serde_json::json!("list");
            }
            let mut invalid: BundlePlan = serde_json::from_value(invalid).unwrap();
            invalid.plan_id = identity(&invalid).unwrap();
            assert_eq!(validate(&invalid).unwrap_err(), INVALID);
        }
    }
}

fn native_plan(command: &Converter, input: &Path, output_root: &Path) -> BundlePlan {
    let outputs = [
        BundleOutputIntent {
            id: "explicit-list".into(),
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            drms: Vec::new(),
            conformance: ManifestValidation::None,
            processing: None,
            dash_addressing: Some(DashAddressing::List),
        },
        BundleOutputIntent {
            id: "default-template".into(),
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            drms: Vec::new(),
            conformance: ManifestValidation::None,
            processing: None,
            dash_addressing: None,
        },
    ];
    command
        .create_bundle_plan(BundlePlanRequest {
            input: VerificationSource {
                input,
                input_keys: &crate::conversion::InputKeys::default(),
                hls_keys: &crate::conversion::HlsInputKeys::default(),
            },
            output_root,
            outputs: &outputs,
        })
        .unwrap()
}

#[test]
fn native_bundle_resolves_each_override_once_and_restores_saved_defaults() {
    let root = crate::native::private_tempdir().unwrap();
    let input = plan::options_tests::native_input(root.path());
    let output_root = root.path().join("result");
    let (command, events) = plan::options_tests::native_command();
    let saved = native_plan(&command, &input, &output_root);
    assert_eq!(saved.schema_version, 8);
    for (id, expected) in [
        ("explicit-list", DashAddressing::List),
        ("default-template", DashAddressing::TemplateDuration),
    ] {
        let selected = saved.output_options(id).unwrap();
        assert_eq!(selected.dash_addressing, Some(expected));
        assert!(selected.layout.is_some());
    }
    let frozen = command.reopen_source(&saved.source).unwrap();
    let current = command
        .using_frozen_source(&frozen)
        .with_layout(crate::conversion::layout::LayoutMode::Normalized)
        .with_dash_addressing(DashAddressing::Base);
    current
        .validate_bundle_source(
            &saved,
            VerificationSource {
                input: frozen.entrypoint(),
                input_keys: &crate::conversion::InputKeys::default(),
                hls_keys: &crate::conversion::HlsInputKeys::default(),
            },
        )
        .unwrap();
    plan::options_tests::assert_no_tools(&events);
    assert!(!output_root.exists());
}

#[test]
fn native_bundle_rejects_rehashed_paths_before_creating_any_output_stage() {
    let root = crate::native::private_tempdir().unwrap();
    let input = plan::options_tests::native_input(root.path());
    let output_root = root.path().join("result");
    let (command, events) = plan::options_tests::native_command();
    let mut saved = native_plan(&command, &input, &output_root);
    let setting = &mut saved.output_options.as_mut().unwrap()[0];
    let map = setting.options.layout.as_mut().unwrap();
    map.tracks[0].media_directory = "different-directory".into();
    map.tracks[0].playlist = "different-directory/playlist.m3u8".into();
    saved.plan_id = identity(&saved).unwrap();
    validate(&saved).unwrap();
    assert_eq!(
        command
            .execute_bundle_plan(
                &saved,
                &BundleOperatorKeys::default(),
                &ManifestValidationTools::default(),
            )
            .unwrap_err()
            .0,
        "execution-plan-output-options-changed"
    );
    plan::options_tests::assert_no_tools(&events);
    assert!(!output_root.exists());
    assert!(input.exists());
}
