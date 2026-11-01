//! Bundle planning and saved replay share the freshly qualified codec restriction.
use super::*;
use crate::conversion::plan::vp9_tests::{command, no_tools, output_keys, source};
use crate::conversion::{HlsInputKeys, InputKeys};

#[test]
fn vp9_cbcs_bundle_plans_and_valid_saved_replay_refuse_before_publication() {
    let root = crate::native::private_tempdir().unwrap();
    let source = source();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let input = VerificationSource {
        input: &source,
        input_keys: &empty,
        hls_keys: &hls,
    };
    let (command, events) = command();
    let output_root = root.path().join("result");
    let mut outputs = [BundleOutputIntent {
        id: "dash".into(),
        format: OutputFormat::Dash,
        target: TargetProtection::Cbcs,
        drms: Vec::new(),
        conformance: ManifestValidation::None,
        processing: None,
        dash_addressing: None,
    }];
    assert_eq!(
        command
            .create_bundle_plan(BundlePlanRequest {
                input,
                output_root: &output_root,
                outputs: &outputs,
            })
            .unwrap_err(),
        super::super::super::vp9::PROTECTION_NOT_SUPPORTED
    );
    assert!(!output_root.exists());
    outputs[0].target = TargetProtection::Cenc;
    let mut plan = command
        .create_bundle_plan(BundlePlanRequest {
            input,
            output_root: &output_root,
            outputs: &outputs,
        })
        .unwrap();
    plan.outputs[0].target = TargetProtection::Cbcs;
    plan.plan_id = identity(&plan).unwrap();
    validate(&plan).unwrap();
    let saved = root.path().join("bundle.plan.json");
    super::super::write_bundle(&saved, &plan).unwrap();
    let super::super::SavedPlan::Bundle(reopened) = super::super::read_saved(&saved).unwrap()
    else {
        panic!("bundle plan expected");
    };
    let bytes = std::fs::read(&saved).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("has_vp9"));
    let mut keys = BundleOperatorKeys::default();
    keys.outputs.insert("dash".into(), output_keys());
    assert_eq!(
        command
            .execute_bundle_plan(&reopened, &keys, &ManifestValidationTools::default())
            .unwrap_err(),
        super::super::super::vp9::PROTECTION_NOT_SUPPORTED
    );
    assert!(!output_root.exists());
    assert_eq!(std::fs::read(saved).unwrap(), bytes);
    no_tools(&events);
}
